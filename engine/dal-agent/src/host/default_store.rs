//! Host-side persistence for `Save::SessionAndDefault` outcomes.
//!
//! The actor journals first; only after the journal receipt lands does it
//! call [`persist_default`] with the applied setting. The writer fresh-reads
//! the user `dal.toml`, edits it in place with the [`Config`] helpers (so
//! comments and unrelated tables survive), validates the result, publishes
//! it atomically, and only then refreshes the in-memory snapshot. Any file
//! failure leaves the original file in place, keeps the already-journaled
//! session as is, and surfaces a distinctly-worded [`DefaultWriteError`]:
//! the gated case names the remote boundary, every other failure starts
//! with `default write failed`, never with the journal wording, so the
//! command boundary maps it to `Failed` with the source intact.

use std::sync::Arc;

use super::{DefaultAuthority, HostShared};
use dal_core::{
    ApprovalMode, Command, Config, ConfigError, ConfigProduct, Mode, ModelRoute, Save,
    ThinkingLevel,
};

/// One applied setting awaiting default persistence after its journal receipt.
#[derive(Clone, Debug)]
pub(crate) enum DefaultUpdate {
    /// A `SetMode` outcome with `Save::SessionAndDefault`.
    Mode(Mode),
    /// A `SetThinking` outcome with `Save::SessionAndDefault`.
    Thinking(ThinkingLevel),
    /// A `SetApproval` outcome with `Save::SessionAndDefault`.
    Approval(ApprovalMode),
    /// A `SetModel` outcome with `Save::SessionAndDefault`.
    Model(ModelRoute),
}

impl DefaultUpdate {
    /// The default a submitted command asks to persist: one of the four
    /// setting commands carrying `Save::SessionAndDefault`, else `None`.
    pub(crate) fn requested(command: &Command) -> Option<Self> {
        match command {
            Command::SetMode {
                mode,
                save: Save::SessionAndDefault,
            } => Some(Self::Mode(*mode)),
            Command::SetThinking {
                level,
                save: Save::SessionAndDefault,
            } => Some(Self::Thinking(*level)),
            Command::SetApproval {
                mode,
                save: Save::SessionAndDefault,
            } => Some(Self::Approval(*mode)),
            Command::SetModel {
                model,
                save: Save::SessionAndDefault,
            } => Some(Self::Model(model.clone())),
            _ => None,
        }
    }
}

/// Why a default write did not land.
///
/// Every variant renders product text: `Gated` names the remote boundary,
/// all other failures start with `default write failed`, never with the
/// journal wording, so callers can split the failure source.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub(crate) enum DefaultWriteError {
    /// This host must not write defaults: remote and serve hosts never
    /// rewrite the server file, even on loopback.
    #[error("remote sessions do not persist defaults")]
    Gated,
    /// The bounded file IO did not finish in time. The gated write keeps
    /// running on the blocking pool and may still land, so the file and
    /// snapshot can change after this error; a later write serializes
    /// behind it.
    #[error("default write failed: timed out after 30s")]
    Timeout,
    /// The file could not be read, parsed, validated, or replaced; the
    /// original file is preserved and the detail follows the prefix.
    #[error("default write failed: {message}")]
    Write {
        /// The io, parse, or validation detail.
        message: Box<str>,
    },
}

/// The file IO budget for one default write: well above a local
/// read-edit-write, well below any session reply deadline.
const WRITE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// Maps this host's product name to its configuration product for reload
/// validation.
fn config_product(name: &str) -> ConfigProduct {
    if name == "dalgona" {
        ConfigProduct::Dalgona
    } else {
        ConfigProduct::Dalgon
    }
}

/// Persists one journaled setting as the product default.
///
/// Fails closed without touching the file when this host has no local
/// authority. Otherwise serializes with concurrent sessions, fresh-reads
/// the user TOML, applies `update` on a scratch copy of the live snapshot
/// (so a file failure cannot mutate live memory), reload-validates the
/// rendered document, publishes it atomically as owner-only, and only then
/// swaps the live snapshot so subsequently opened sessions seed from it.
/// The whole cycle runs on the blocking pool under the writer gate, so a
/// lapse reports [`DefaultWriteError::Timeout`] without stalling an async
/// executor while the gated write still finishes in the background. The
/// gate parks its waiters on the blocking pool, bounded by the host actor's
/// one-command-per-session serialization.
///
/// # Errors
/// Returns [`DefaultWriteError::Gated`] without touching the file when this
/// host must not write, [`DefaultWriteError::Timeout`] when the bounded IO
/// lapses, or [`DefaultWriteError::Write`] with the detail when the file
/// cannot be read, parsed, validated, or replaced.
pub(crate) async fn persist_default(
    shared: &Arc<HostShared>,
    update: &DefaultUpdate,
) -> Result<(), DefaultWriteError> {
    let DefaultAuthority::Local { path } = &shared.default_authority else {
        return Err(DefaultWriteError::Gated);
    };
    let path = path.clone();
    let update = update.clone();
    let shared = shared.clone();
    // The whole read-edit-write cycle runs on the blocking pool: the file
    // syncs and rename below never block an async executor, and the timeout
    // wraps a real await point, so a slow device cannot stall the runtime.
    // A lapsed write keeps its gate until the file work ends, so a later
    // writer never races a stale temp file onto the fresh one. Concurrent
    // writers queue as tasks whose `blocking_lock` parks at most one
    // blocking-pool thread each; the host actor serializes session commands,
    // so in practice at most one parked thread exists per in-flight default
    // write, and the park ends when the gate holder's file work ends.
    let write = tokio::task::spawn_blocking(move || {
        let _guard = shared.default_write.blocking_lock();
        write_update(&shared, &path, &update)
    });
    match tokio::time::timeout(WRITE_TIMEOUT, write).await {
        Ok(outcome) => outcome.map_err(|error| DefaultWriteError::Write {
            message: format!("default write task failed: {error}").into(),
        })?,
        Err(_) => Err(DefaultWriteError::Timeout),
    }
}

/// Reads, edits, validates, publishes, and commits one default update.
/// Runs on the blocking pool inside the gate held by [`persist_default`];
/// every call here blocks the calling thread, never an async executor.
fn write_update(
    shared: &HostShared,
    path: &std::path::Path,
    update: &DefaultUpdate,
) -> Result<(), DefaultWriteError> {
    let live = shared
        .defaults
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    let user_toml = match std::fs::read_to_string(path) {
        Ok(text) => Some(text),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(DefaultWriteError::Write {
                message: format!("cannot read {}: {error}", path.display()).into(),
            });
        }
    };
    let mut scratch = live;
    let updated = apply_update(&mut scratch, update, user_toml.as_deref()).map_err(|error| {
        DefaultWriteError::Write {
            message: error.to_string().into(),
        }
    })?;
    // Reload-validate the rendered document before touching the file or
    // memory: a rejected document preserves the original file.
    Config::validate_user_toml(config_product(shared.product_name), &updated).map_err(|error| {
        DefaultWriteError::Write {
            message: error.to_string().into(),
        }
    })?;
    if let Some(parent) = path.parent() {
        dal_store::create_private_dir_all(parent).map_err(|error| DefaultWriteError::Write {
            message: format!("cannot create {}: {error}", parent.display()).into(),
        })?;
    }
    dal_store::write_atomic(path, updated.as_bytes(), dal_store::FileMode::Mode0600).map_err(
        |error| DefaultWriteError::Write {
            message: format!("cannot save {}: {error}", path.display()).into(),
        },
    )?;
    // Commit the scratch configuration that rendered the published file, so
    // the next opened session seeds from the committed defaults. Brief
    // write lock, no await inside.
    *shared
        .defaults
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner) = scratch;
    Ok(())
}

/// Applies one update to the scratch configuration, rendering the new user
/// TOML. Pure CPU over already-validated values; file and memory are
/// untouched on error.
fn apply_update(
    scratch: &mut Config,
    update: &DefaultUpdate,
    user_toml: Option<&str>,
) -> Result<String, ConfigError> {
    match update {
        DefaultUpdate::Mode(mode) => scratch.update_default_mode(*mode, user_toml),
        DefaultUpdate::Thinking(level) => scratch.update_default_thinking(*level, user_toml),
        DefaultUpdate::Approval(mode) => scratch.update_default_approval(*mode, user_toml),
        DefaultUpdate::Model(route) => scratch.update_default_model(route, user_toml),
    }
}

#[cfg(test)]
mod tests {
    use dal_core::{
        ApprovalMode, Command, Config, ConfigProduct, Family, Mode, ModelRoute, Save, ThinkingLevel,
    };

    use super::{DefaultUpdate, apply_update};

    const USER_TOML: &str = "# kept user comment\n";

    fn load(root: &std::path::Path, user: &str) -> Config {
        Config::load(ConfigProduct::Dalgon, root, "", Some(user)).expect("fixture config loads")
    }

    /// Applies `update`, then proves the rendered file keeps the user
    /// comment, validates, and reloads as the committed scratch snapshot.
    fn restart(update: &DefaultUpdate) -> (Config, Config) {
        let root = tempfile::tempdir().expect("fixture data root");
        let mut scratch = load(root.path(), USER_TOML);
        let rendered =
            apply_update(&mut scratch, update, Some(USER_TOML)).expect("the update renders");
        assert!(rendered.contains("# kept user comment"), "{rendered}");
        Config::validate_user_toml(ConfigProduct::Dalgon, &rendered)
            .expect("the rendered document validates");
        (scratch, load(root.path(), &rendered))
    }

    #[test]
    fn only_session_and_default_settings_request_a_default() {
        let route = ModelRoute::Api {
            family: Family::Mistral,
            model: "mistral/large".into(),
            provider: None,
        };
        let only = [
            Command::SetMode {
                mode: Mode::EvalFirst,
                save: Save::SessionOnly,
            },
            Command::SetThinking {
                level: ThinkingLevel::High,
                save: Save::SessionOnly,
            },
            Command::SetApproval {
                mode: ApprovalMode::Edits,
                save: Save::SessionOnly,
            },
            Command::SetModel {
                model: route.clone(),
                save: Save::SessionOnly,
            },
            Command::SetScopedModels(Vec::new()),
        ];
        for command in &only {
            assert!(DefaultUpdate::requested(command).is_none(), "{command:?}");
        }
        assert!(matches!(
            DefaultUpdate::requested(&Command::SetMode {
                mode: Mode::EvalFirst,
                save: Save::SessionAndDefault,
            }),
            Some(DefaultUpdate::Mode(Mode::EvalFirst))
        ));
        assert!(matches!(
            DefaultUpdate::requested(&Command::SetThinking {
                level: ThinkingLevel::Max,
                save: Save::SessionAndDefault,
            }),
            Some(DefaultUpdate::Thinking(ThinkingLevel::Max))
        ));
        assert!(matches!(
            DefaultUpdate::requested(&Command::SetApproval {
                mode: ApprovalMode::All,
                save: Save::SessionAndDefault,
            }),
            Some(DefaultUpdate::Approval(ApprovalMode::All))
        ));
        assert!(matches!(
            DefaultUpdate::requested(&Command::SetModel {
                model: route.clone(),
                save: Save::SessionAndDefault,
            }),
            Some(DefaultUpdate::Model(saved)) if saved == route
        ));
    }

    #[test]
    fn every_family_route_survives_a_restart_exactly() {
        let families = [
            Family::Chat,
            Family::Responses,
            Family::Codex,
            Family::Anthropic,
            Family::Bedrock,
            Family::Cursor,
            Family::Devin,
            Family::Droid,
            Family::GitLab,
            Family::GoogleGenerativeAi,
            Family::GoogleVertex,
            Family::GoogleGeminiCli,
            Family::GoogleAntigravity,
            Family::Mistral,
            Family::Ollama,
            Family::PiMessages,
        ];
        for family in families {
            let route = ModelRoute::Api {
                family,
                model: "provider/model-1".into(),
                provider: None,
            };
            let (scratch, reloaded) = restart(&DefaultUpdate::Model(route.clone()));
            assert_eq!(scratch.default_model_route(), Some(route.clone()));
            assert_eq!(reloaded.default_model_route(), Some(route), "{family:?}");
        }
    }

    #[test]
    fn a_route_with_provider_survives_a_restart_beside_a_providerless_mirror() {
        let route = ModelRoute::Api {
            family: Family::Responses,
            model: "gpt-5".into(),
            provider: Some("acme-proxy".into()),
        };
        let (scratch, reloaded) = restart(&DefaultUpdate::Model(route.clone()));
        assert_eq!(scratch.default_model_route(), Some(route.clone()));
        assert_eq!(reloaded.default_model_route(), Some(route));
        // The legacy `model` mirror never gains a provider segment; the
        // structured table is the only form that carries it.
        assert_eq!(
            scratch.model().map(ToString::to_string),
            Some("openai-responses/gpt-5".into())
        );
        let (scratch, reloaded) = restart(&DefaultUpdate::Model(ModelRoute::Api {
            family: Family::Chat,
            model: "gpt-5".into(),
            provider: None,
        }));
        assert_eq!(
            scratch.default_model_route(),
            Some(ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-5".into(),
                provider: None,
            })
        );
        assert_eq!(
            reloaded.default_model_route(),
            Some(ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-5".into(),
                provider: None,
            })
        );
    }

    #[test]
    fn every_mode_thinking_and_approval_survives_a_restart() {
        for mode in [Mode::Normal, Mode::EvalFirst, Mode::EvalOnly] {
            let (scratch, reloaded) = restart(&DefaultUpdate::Mode(mode));
            assert_eq!((scratch.mode(), reloaded.mode()), (mode, mode));
        }
        for level in [
            ThinkingLevel::Off,
            ThinkingLevel::Minimal,
            ThinkingLevel::Low,
            ThinkingLevel::Medium,
            ThinkingLevel::High,
            ThinkingLevel::Xhigh,
            ThinkingLevel::Max,
        ] {
            let (scratch, reloaded) = restart(&DefaultUpdate::Thinking(level));
            assert_eq!((scratch.thinking(), reloaded.thinking()), (level, level));
        }
        for approval in [ApprovalMode::Ask, ApprovalMode::Edits, ApprovalMode::All] {
            let (scratch, reloaded) = restart(&DefaultUpdate::Approval(approval));
            assert_eq!(
                (scratch.approval(), reloaded.approval()),
                (approval, approval)
            );
        }
    }

    /// Real-host proofs: restart, same-host visibility, failure split, and
    /// concurrent saves, through `Host::start_with_defaults` and `submit`.
    mod host {
        use std::collections::BTreeMap;
        use std::ffi::OsString;
        use std::path::{Path, PathBuf};
        use std::time::Duration;

        use dal_core::{
            ClientId, Command, Config, ConfigProduct, Family, Mode, ModelRoute, PageReq, Save,
            ThinkingLevel, Workspace,
        };

        use crate::{Agent, AgentError, DefaultAuthority, Env, Host, Product, SessionRef};

        const CLOSE: Duration = Duration::from_secs(5);
        const USER_TOML: &str = "# kept user comment\nmodel = \"openai/gpt-6-luna\"\n";

        struct Rig {
            tmp: tempfile::TempDir,
            user: PathBuf,
        }

        impl Rig {
            fn new() -> Self {
                let tmp = tempfile::tempdir().expect("tempdir");
                std::fs::create_dir_all(tmp.path().join("data")).expect("data directory");
                std::fs::create_dir_all(tmp.path().join("workspace")).expect("workspace directory");
                let user = tmp.path().join("config").join("dal.toml");
                std::fs::create_dir_all(user.parent().expect("config parent"))
                    .expect("config directory");
                std::fs::write(&user, USER_TOML).expect("user config");
                Self { tmp, user }
            }

            fn data(&self) -> PathBuf {
                self.tmp.path().join("data")
            }

            /// The configuration a fresh process would load from the file now.
            fn reload(&self) -> Config {
                let text = std::fs::read_to_string(&self.user).expect("user config reads");
                Config::load(ConfigProduct::Dalgon, &self.data(), "", Some(text.as_str()))
                    .expect("saved config loads")
            }

            async fn start(&self, authority: DefaultAuthority) -> Host {
                let product = Product {
                    name: "dal",
                    data_root: self.data(),
                    defaults: "",
                    extensions: Vec::new(),
                    bundled: Vec::new(),
                };
                let env = Env {
                    vars: BTreeMap::from([
                        (OsString::from("OPENAI_API_KEY"), OsString::from("sk-test")),
                        (OsString::from("HOME"), self.tmp.path().join("home").into()),
                        (
                            OsString::from("XDG_CACHE_HOME"),
                            self.tmp.path().join("cache").into(),
                        ),
                    ]),
                    cwd: self.tmp.path().join("workspace"),
                    sandbox_helper: None,
                };
                Host::start_with_defaults(product, self.reload(), env, authority)
                    .await
                    .expect("host")
            }

            fn local(&self) -> DefaultAuthority {
                DefaultAuthority::Local {
                    path: self.user.clone(),
                }
            }
        }

        async fn open(host: &Host, rig: &Rig, client: &str) -> Agent {
            let workspace = Workspace::new(rig.tmp.path().join("workspace")).expect("workspace");
            host.open(
                SessionRef::New {
                    workspace,
                    name: None,
                },
                ClientId::new(client),
            )
            .await
            .expect("session")
        }

        fn thinking(agent: &Agent) -> ThinkingLevel {
            agent
                .view(PageReq::default())
                .expect("view")
                .settings
                .thinking
        }

        fn other_level(level: ThinkingLevel) -> ThinkingLevel {
            if level == ThinkingLevel::High {
                ThinkingLevel::Low
            } else {
                ThinkingLevel::High
            }
        }

        fn persistence_failure(result: Result<dal_core::Reply, AgentError>) -> Box<str> {
            match result {
                Err(AgentError::PersistenceFailed { message }) => message,
                other => panic!("expected a persistence failure, got {other:?}"),
            }
        }

        #[cfg(unix)]
        fn assert_private(path: &Path) {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(path)
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600, "{}", path.display());
        }

        #[cfg(not(unix))]
        fn assert_private(_path: &Path) {}

        #[tokio::test]
        async fn saved_default_survives_restart_and_reaches_only_later_sessions() {
            let rig = Rig::new();
            let host = rig.start(rig.local()).await;
            let saver = open(&host, &rig, "saver").await;
            let running = open(&host, &rig, "running").await;
            let before = thinking(&running);
            let target = other_level(before);
            saver
                .submit(Command::SetThinking {
                    level: target,
                    save: Save::SessionAndDefault,
                })
                .await
                .expect("the default saves");
            let route = ModelRoute::Api {
                family: Family::Mistral,
                model: "mistral/mistral-large".into(),
                provider: None,
            };
            saver
                .submit(Command::SetModel {
                    model: route.clone(),
                    save: Save::SessionAndDefault,
                })
                .await
                .expect("the model default saves");
            assert_eq!(thinking(&saver), target);
            assert_eq!(
                thinking(&running),
                before,
                "a running session keeps its open defaults"
            );
            let later = open(&host, &rig, "later").await;
            assert_eq!(thinking(&later), target, "a later session sees the commit");
            let text = std::fs::read_to_string(&rig.user).expect("user config reads");
            assert!(text.contains("# kept user comment"), "{text}");
            assert_private(&rig.user);
            let reloaded = rig.reload();
            assert_eq!(reloaded.thinking(), target);
            assert_eq!(reloaded.default_model_route(), Some(route));
            let _ = host.shutdown(CLOSE).await;

            let restarted = rig.start(rig.local()).await;
            let fresh = open(&restarted, &rig, "restarted").await;
            assert_eq!(thinking(&fresh), target, "the default survives a restart");
            let _ = restarted.shutdown(CLOSE).await;
        }

        fn selected_model(agent: &Agent) -> Option<ModelRoute> {
            let view = agent.view(PageReq::default()).expect("view");
            view.settings.model
        }

        fn proxy_route(provider: Option<&str>) -> ModelRoute {
            ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
                provider: provider.map(Into::into),
            }
        }

        #[tokio::test]
        async fn a_saved_provider_route_survives_a_restart_with_a_providerless_mirror() {
            let rig = Rig::new();
            let host = rig.start(rig.local()).await;
            let saver = open(&host, &rig, "saver").await;
            let route = proxy_route(Some("acme-proxy"));
            saver
                .submit(Command::SetModel {
                    model: route.clone(),
                    save: Save::SessionAndDefault,
                })
                .await
                .expect("the provider default saves");
            let text = std::fs::read_to_string(&rig.user).expect("user config reads");
            assert!(text.contains("# kept user comment"), "{text}");
            assert!(text.contains("provider = \"acme-proxy\""), "{text}");
            assert!(
                text.contains("model = \"openai-chat/gpt-6-luna\""),
                "the legacy mirror keeps the family spelling: {text}"
            );
            assert!(
                !text.contains("acme-proxy/gpt-6-luna"),
                "the legacy mirror never gains a provider segment: {text}"
            );
            assert_eq!(rig.reload().default_model_route(), Some(route));
            let _ = host.shutdown(CLOSE).await;

            let restarted = rig.start(rig.local()).await;
            let fresh = open(&restarted, &rig, "restarted").await;
            assert_eq!(
                selected_model(&fresh),
                None,
                "a session opened after the restart inherits the saved default"
            );
            let _ = restarted.shutdown(CLOSE).await;
        }

        #[tokio::test]
        async fn a_providerless_default_save_drops_the_previous_provider_member() {
            let rig = Rig::new();
            let host = rig.start(rig.local()).await;
            let saver = open(&host, &rig, "saver").await;
            for route in [proxy_route(Some("acme-proxy")), proxy_route(None)] {
                saver
                    .submit(Command::SetModel {
                        model: route,
                        save: Save::SessionAndDefault,
                    })
                    .await
                    .expect("the default saves");
            }
            let text = std::fs::read_to_string(&rig.user).expect("user config reads");
            assert!(
                !text.contains("provider"),
                "a provider-less save leaves no stale provider member: {text}"
            );
            assert_eq!(rig.reload().default_model_route(), Some(proxy_route(None)));
            let _ = host.shutdown(CLOSE).await;
        }

        #[tokio::test]
        async fn disabled_host_fails_gated_without_writing_and_keeps_the_session() {
            let rig = Rig::new();
            let host = rig.start(DefaultAuthority::Disabled).await;
            let agent = open(&host, &rig, "remote").await;
            let message = persistence_failure(
                agent
                    .submit(Command::SetMode {
                        mode: Mode::EvalFirst,
                        save: Save::SessionAndDefault,
                    })
                    .await,
            );
            assert_eq!(message.as_ref(), "remote sessions do not persist defaults");
            let view = agent.view(PageReq::default()).expect("view");
            assert_eq!(
                view.settings.mode,
                Mode::EvalFirst,
                "the journaled session stays"
            );
            assert_eq!(
                std::fs::read_to_string(&rig.user).expect("user config reads"),
                USER_TOML,
                "the server file is untouched"
            );
            let _ = host.shutdown(CLOSE).await;
        }

        #[tokio::test]
        async fn file_failure_is_distinct_from_journal_failure_and_keeps_the_session() {
            let rig = Rig::new();
            // A directory where the file belongs: the fresh read fails.
            let blocked = rig.tmp.path().join("blocked");
            std::fs::create_dir_all(&blocked).expect("blocking directory");
            let host = rig.start(DefaultAuthority::Local { path: blocked }).await;
            let agent = open(&host, &rig, "blocked").await;
            let target = other_level(thinking(&agent));
            let message = persistence_failure(
                agent
                    .submit(Command::SetThinking {
                        level: target,
                        save: Save::SessionAndDefault,
                    })
                    .await,
            );
            assert!(message.starts_with("default write failed"), "{message}");
            assert!(!message.contains("journal"), "{message}");
            assert_eq!(
                thinking(&agent),
                target,
                "no rollback of the journaled session"
            );
            let _ = host.shutdown(CLOSE).await;
        }

        #[tokio::test]
        async fn session_only_saves_never_write() {
            let rig = Rig::new();
            let host = rig.start(rig.local()).await;
            let agent = open(&host, &rig, "session-only").await;
            let target = other_level(thinking(&agent));
            agent
                .submit(Command::SetThinking {
                    level: target,
                    save: Save::SessionOnly,
                })
                .await
                .expect("session-only applies");
            assert_eq!(
                std::fs::read_to_string(&rig.user).expect("user config reads"),
                USER_TOML
            );
            let _ = host.shutdown(CLOSE).await;
        }

        #[tokio::test]
        async fn concurrent_saves_from_two_sessions_both_land() {
            let rig = Rig::new();
            let host = rig.start(rig.local()).await;
            let first = open(&host, &rig, "first").await;
            let second = open(&host, &rig, "second").await;
            let target = other_level(thinking(&first));
            let (thinking_saved, mode_saved) = tokio::join!(
                first.submit(Command::SetThinking {
                    level: target,
                    save: Save::SessionAndDefault,
                }),
                second.submit(Command::SetMode {
                    mode: Mode::EvalFirst,
                    save: Save::SessionAndDefault,
                }),
            );
            thinking_saved.expect("the thinking default saves");
            mode_saved.expect("the mode default saves");
            let reloaded = rig.reload();
            assert_eq!(reloaded.thinking(), target, "neither write lost the other");
            assert_eq!(
                reloaded.mode(),
                Mode::EvalFirst,
                "neither write lost the other"
            );
            let later = open(&host, &rig, "later").await;
            let later_view = later.view(PageReq::default()).expect("view");
            assert_eq!(
                later_view.settings.thinking, target,
                "the live snapshot keeps the thinking default the concurrent mode write landed"
            );
            assert_eq!(
                later_view.settings.mode,
                Mode::EvalFirst,
                "the live snapshot keeps the mode default the concurrent thinking write landed"
            );
            let _ = host.shutdown(CLOSE).await;
        }
    }
}
