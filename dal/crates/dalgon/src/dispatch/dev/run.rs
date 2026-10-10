//! `dalgon dev run` — executes scripted headless-session scenarios.
//!
//! A scenario is one step JSON object per nonblank line. The run drives a real
//! `Host` and session inside a fresh run root (data root plus workspace) that
//! is deleted on success and kept on `--keep`, so every assertion reads the
//! same state a client or post-mortem would see.

use std::{
    collections::VecDeque,
    fmt::Write as _,
    io::Write,
    path::{Component, Path, PathBuf},
    process::ExitCode,
    sync::Arc,
    time::{Duration, Instant},
};

use dal_agent::{Agent, Delivery, Env, Host, SessionRef, Subscription};
use dal_core::{
    Answer, ApprovalMode, CancelScope, ClientId, Command, Config, ConfigProduct, Expect,
    ExportFormat, ModelRoute, Part, RawJson, Reply, RequestId, Save, SessionId, ThinkingLevel,
    TurnId, Update, UpdateKind, Workspace,
};
use dal_provider::Script;
use serde::Deserialize;

use super::{DevError, util};
use crate::{BuildCx, VarsMap, cli, exit};

const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// The bundled `devprobe` extension: echo, env/state/ask probes, a command,
/// and `before_turn`/`tool_call` hooks — enough surface to exercise dispatch,
/// service grants, and requests without hand-writing fixture plugins.
const DEVPROBE_STAR: &str = include_str!("devprobe.star");

/// Runs the scenario file to completion or its first failing step.
pub(super) async fn run(
    args: &cli::DevRunArgs,
    vars: VarsMap,
    cwd: PathBuf,
    helper: Option<PathBuf>,
) -> Result<ExitCode, DevError> {
    let text = std::fs::read_to_string(&args.file).map_err(|source| DevError::Read {
        path: args.file.display().to_string(),
        source,
    })?;
    let scenario_dir = args
        .file
        .parent()
        .filter(|dir| !dir.as_os_str().is_empty())
        .map_or_else(|| cwd.clone(), |dir| resolve(&cwd, dir));
    let mut run = RunCx::new(
        scenario_dir,
        vars,
        helper,
        args.keep,
        args.root.clone(),
        args.consent,
    )?;

    let stdout = std::io::stdout();
    let mut out = stdout.lock();
    let mut done = 0usize;
    let mut result = Ok(());
    for (index, line) in text.lines().enumerate() {
        let line_no = index + 1;
        if line.trim().is_empty() {
            continue;
        }
        let step = match Step::decode(line, &args.file, line_no) {
            Ok(step) => step,
            Err(error) => {
                result = Err(error);
                break;
            }
        };
        if let Err(error) = run.step(step, line_no, &mut out).await {
            result = Err(error);
            break;
        }
        done += 1;
    }
    // The host must close before the run root is removed: live actors keep
    // journals open and would race the deletion (or leak past the report).
    run.finish(&mut out).await;
    result?;
    let _ = writeln!(out, "scenario passed ({done} steps)");
    Ok(exit::code(exit::ExitKind::Success))
}

/// The `update` expectation payload.
struct UpdateSpec {
    kind: String,
    contains: Option<String>,
    not_contains: Option<String>,
    timeout_ms: Option<u64>,
}

/// The `request` expectation payload.
struct RequestSpec {
    kind: String,
    answer: Option<Answer>,
    timeout_ms: Option<u64>,
}

/// The `journal` expectation payload.
struct JournalSpec {
    kind: String,
    at_least: usize,
}

/// The `file` expectation payload.
struct FileSpec {
    path: PathBuf,
    contains: Option<String>,
    equals: Option<String>,
}

/// A prompt's session-state gate, resolved against the run's last turn.
enum PromptGate {
    Idle,
    Previous,
}

/// A session shape the `session` step selects.
enum SessionKind {
    New(Option<String>),
    Resume(String),
    Continue,
    Ephemeral,
}

/// One decoded scenario step; paths resolve at execution time.
enum Step {
    Provider(ProviderWire),
    Config(ConfigWire),
    Plugin(PluginWire),
    Session(SessionKind),
    Write {
        path: PathBuf,
        text: String,
    },
    Prompt {
        text: String,
        gate: PromptGate,
    },
    Steer(String),
    FollowUp(String),
    Run {
        name: String,
        args: String,
    },
    Reload,
    Cancel,
    SetModel(String),
    SetApproval(String),
    SetThinking(String),
    Compact(Option<String>),
    Rename(String),
    Export {
        path: Option<PathBuf>,
        format: ExportFormat,
    },
    Answer {
        kind: Option<String>,
        answer: Answer,
    },
    ExpectUpdate(UpdateSpec),
    ExpectRequest(RequestSpec),
    ExpectJournal(JournalSpec),
    ExpectFile(FileSpec),
    ExpectQuiet(u64),
    Sleep(u64),
    Comment(String),
}

impl Step {
    /// Decodes one scenario line: a single-key step JSON object.
    fn decode(line: &str, file: &Path, line_no: usize) -> Result<Self, DevError> {
        let invalid = |detail: String| DevError::Scenario {
            path: file.display().to_string(),
            line: line_no,
            detail,
        };
        let wire: StepWire = sonic_rs::from_str(line)
            .map_err(|error| invalid(format!("not a step object: {error}")))?;
        wire.decode(&invalid)
    }
}

/// The process state one scenario run owns.
struct RunCx {
    root_path: PathBuf,
    root: Option<PathBuf>,
    keep: bool,
    data_root: PathBuf,
    workspace: PathBuf,
    workspace_canonical: PathBuf,
    scenario_dir: PathBuf,
    vars: VarsMap,
    helper: Option<PathBuf>,
    provider_fixture: Option<PathBuf>,
    config_text: Vec<String>,
    plugins: Vec<String>,
    session_kind: SessionKind,
    consent: bool,
    host: Option<Host>,
    agent: Option<Agent>,
    subscription: Option<Subscription>,
    open_requests: VecDeque<(RequestId, String)>,
    last_turn: Option<TurnId>,
}

impl RunCx {
    /// Builds the run root under the system temp dir, or adopts `root`.
    ///
    /// An adopted root is never deleted: it is how `resume` and `continue`
    /// scenarios find a populated store — pass the `kept run root` line of
    /// an earlier `--keep` run.
    fn new(
        scenario_dir: PathBuf,
        vars: VarsMap,
        helper: Option<PathBuf>,
        keep: bool,
        root: Option<PathBuf>,
        consent: bool,
    ) -> Result<Self, DevError> {
        let root_path = root.clone().unwrap_or_else(|| {
            std::env::temp_dir().join(format!(
                "dal-dev-{}-{}",
                std::process::id(),
                SessionId::new_v7()
            ))
        });
        let data_root = root_path.join("data");
        let workspace = root_path.join("work");
        for dir in [&root_path, &data_root, &workspace] {
            std::fs::create_dir_all(dir).map_err(|source| DevError::Write {
                path: dir.display().to_string(),
                source,
            })?;
        }
        // The containment boundary is canonicalized once: a later write
        // resolves through the workspace's own links, never the strings.
        let workspace_canonical = workspace.canonicalize().map_err(|source| DevError::Read {
            path: workspace.display().to_string(),
            source,
        })?;
        Ok(Self {
            root_path,
            root,
            keep,
            data_root,
            workspace,
            workspace_canonical,
            scenario_dir,
            vars,
            helper,
            provider_fixture: None,
            config_text: Vec::new(),
            plugins: Vec::new(),
            session_kind: SessionKind::New(None),
            consent,
            host: None,
            agent: None,
            subscription: None,
            open_requests: VecDeque::new(),
            last_turn: None,
        })
    }

    /// Executes one decoded step and prints its progress line.
    async fn step(
        &mut self,
        step: Step,
        line: usize,
        out: &mut impl Write,
    ) -> Result<(), DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        match step {
            Step::Provider(wire) => {
                self.require_pre_session(line)?;
                let fixture = self.provider_fixture(wire, &fail)?;
                self.provider_fixture = Some(fixture.clone());
                let _ = writeln!(out, "{line:>4}  provider <- {}", fixture.display());
            }
            Step::Config(wire) => {
                self.require_pre_session(line)?;
                let text = match wire {
                    ConfigWire::Text(text) => text,
                    ConfigWire::File(wire) => {
                        let file = wire.file;
                        let path = resolve(&self.scenario_dir, &file);
                        std::fs::read_to_string(&path).map_err(|source| DevError::Read {
                            path: path.display().to_string(),
                            source,
                        })?
                    }
                };
                self.config_text.push(text);
                // The gate reads the joined config through the real TOML
                // decoder: quoting tricks cannot hide a key, and fragments
                // spread across config steps still resolve. Any authorizing
                // key needs the invoker's --consent.
                if let Some(key) = gated_config_key(&self.config_text.join("\n")) {
                    self.require_consent(line, &format!("a config-declared `{key}`"))?;
                }
                let _ = writeln!(out, "{line:>4}  config");
            }
            Step::Plugin(wire) => {
                self.require_pre_session(line)?;
                let name = self.plugin_step(wire, &fail)?;
                let _ = writeln!(out, "{line:>4}  plugin {name}");
            }
            Step::Session(kind) => {
                self.require_pre_session(line)?;
                self.session_kind = kind;
                let _ = writeln!(out, "{line:>4}  session");
            }
            Step::Write { path, text } => {
                let target = self.inside_workspace(&path).map_err(fail)?;
                if let Some(parent) = target.parent() {
                    std::fs::create_dir_all(parent).map_err(|source| DevError::Write {
                        path: parent.display().to_string(),
                        source,
                    })?;
                }
                std::fs::write(&target, text).map_err(|source| DevError::Write {
                    path: target.display().to_string(),
                    source,
                })?;
                let _ = writeln!(out, "{line:>4}  write {}", path.display());
            }
            Step::Prompt { .. }
            | Step::Steer(..)
            | Step::FollowUp(..)
            | Step::Run { .. }
            | Step::Reload
            | Step::Cancel
            | Step::SetModel(..)
            | Step::SetApproval(..)
            | Step::SetThinking(..)
            | Step::Compact(..)
            | Step::Rename(..)
            | Step::Export { .. }
            | Step::Answer { .. } => {
                let label = self.command_step(step, line).await?;
                let _ = writeln!(out, "{line:>4}  {label}");
            }
            Step::ExpectUpdate(spec) => self.expect_update(&spec, line, out).await?,
            Step::ExpectRequest(spec) => self.expect_request(&spec, line, out).await?,
            Step::ExpectJournal(spec) => self.expect_journal(&spec, line, out)?,
            Step::ExpectFile(spec) => self.expect_file(&spec, line, out)?,
            Step::ExpectQuiet(ms) => {
                if let Some(update) = self.poll(Duration::from_millis(ms), line).await? {
                    let (kind, _) = update_shape(&update);
                    return Err(fail(format!("expected quiet, got update {kind}")));
                }
                let _ = writeln!(out, "{line:>4}  quiet {ms}ms");
            }
            Step::Sleep(ms) => {
                tokio::time::sleep(Duration::from_millis(ms)).await;
                let _ = writeln!(out, "{line:>4}  sleep {ms}ms");
            }
            Step::Comment(text) => {
                let _ = writeln!(out, "{line:>4}  # {text}");
            }
        }
        Ok(())
    }

    /// Executes a session-bound step and returns its progress label.
    async fn command_step(&mut self, step: Step, line: usize) -> Result<String, DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        Ok(match step {
            Step::Prompt { text, gate } => {
                let expect = match gate {
                    PromptGate::Idle => Expect::Idle,
                    PromptGate::Previous => Expect::After(self.last_turn.ok_or_else(|| {
                        fail("prompt `previous` needs an earlier turn".to_owned())
                    })?),
                };
                let reply = self
                    .submit(
                        Command::Prompt {
                            expect,
                            content: vec![Part::Text {
                                text: text.clone().into_boxed_str(),
                            }],
                        },
                        line,
                    )
                    .await?;
                reply_line(&reply)
            }
            Step::Steer(text) => {
                let reply = self
                    .submit(
                        Command::Steer {
                            turn: self.turn(line, "steer")?,
                            content: vec![Part::Text {
                                text: text.into_boxed_str(),
                            }],
                        },
                        line,
                    )
                    .await?;
                format!("steer -> {}", reply_line(&reply))
            }
            Step::FollowUp(text) => {
                let reply = self
                    .submit(
                        Command::FollowUp {
                            turn: self.turn(line, "follow_up")?,
                            content: vec![Part::Text {
                                text: text.into_boxed_str(),
                            }],
                        },
                        line,
                    )
                    .await?;
                format!("follow_up -> {}", reply_line(&reply))
            }
            Step::Run { name, args } => {
                self.require_consent(line, "running a built-in command in-band")?;
                let reply = self
                    .submit(
                        Command::Run {
                            name: name.clone().into_boxed_str(),
                            args: args.into_boxed_str(),
                            expected: None,
                        },
                        line,
                    )
                    .await?;
                format!("run {name} -> {}", reply_line(&reply))
            }
            Step::Reload => {
                let reply = self.submit(Command::ReloadPlugins, line).await?;
                format!("reload -> {}", reply_line(&reply))
            }
            Step::Cancel => {
                let reply = self
                    .submit(
                        Command::Cancel {
                            scope: CancelScope::Turn(self.turn(line, "cancel")?),
                        },
                        line,
                    )
                    .await?;
                format!("cancel -> {}", reply_line(&reply))
            }
            Step::SetModel(..)
            | Step::SetApproval(..)
            | Step::SetThinking(..)
            | Step::Compact(..)
            | Step::Rename(..)
            | Step::Export { .. }
            | Step::Answer { .. } => self.setting_step(step, line).await?,
            _ => unreachable!("the step dispatcher routes only session steps here"),
        })
    }

    /// Executes a settings or answer step and returns its progress label.
    async fn setting_step(&mut self, step: Step, line: usize) -> Result<String, DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        Ok(match step {
            Step::SetModel(id) => {
                let reply = self
                    .submit(
                        Command::SetModel {
                            model: ModelRoute::from_id(&id),
                            save: Save::SessionOnly,
                        },
                        line,
                    )
                    .await?;
                format!("model {id} -> {}", reply_line(&reply))
            }
            Step::SetApproval(mode) => {
                self.require_consent(line, "setting approval in-band")?;
                let mode = approval_mode(&mode).map_err(fail)?;
                let reply = self
                    .submit(
                        Command::SetApproval {
                            mode,
                            save: Save::SessionOnly,
                        },
                        line,
                    )
                    .await?;
                format!("approval -> {}", reply_line(&reply))
            }
            Step::SetThinking(level) => {
                let level = thinking_level(&level).map_err(fail)?;
                let reply = self
                    .submit(
                        Command::SetThinking {
                            level,
                            save: Save::SessionOnly,
                        },
                        line,
                    )
                    .await?;
                format!("thinking -> {}", reply_line(&reply))
            }
            Step::Compact(focus) => {
                let reply = self
                    .submit(
                        Command::Compact {
                            focus: focus.map(String::into_boxed_str),
                        },
                        line,
                    )
                    .await?;
                format!("compact -> {}", reply_line(&reply))
            }
            Step::Rename(name) => {
                let reply = self
                    .submit(Command::Rename(name.into_boxed_str()), line)
                    .await?;
                format!("rename -> {}", reply_line(&reply))
            }
            Step::Export { path, format } => {
                let path = path
                    .map(|path| self.inside_workspace(&path))
                    .transpose()
                    .map_err(fail)?;
                let reply = self.submit(Command::Export { path, format }, line).await?;
                format!("export -> {}", reply_line(&reply))
            }
            Step::Answer { kind, answer } => {
                self.require_consent(line, "answering a request in-band")?;
                let id = match &kind {
                    None => self.open_requests.pop_front(),
                    Some(kind) => self
                        .open_requests
                        .iter()
                        .position(|(_, request_kind)| request_kind == kind)
                        .and_then(|index| self.open_requests.remove(index)),
                }
                .map(|(id, _)| id)
                .ok_or_else(|| fail("no open request to answer".to_owned()))?;
                let agent = self
                    .agent
                    .as_ref()
                    .ok_or_else(|| fail("no session yet".to_owned()))?;
                agent
                    .answer(id, answer)
                    .await
                    .map_err(|error| fail(format!("answer rejected: {error}")))?;
                "answer".to_owned()
            }
            _ => unreachable!("the step dispatcher routes only settings steps here"),
        })
    }

    /// Closes the session and host so the run root can be removed cleanly.
    async fn finish(&mut self, out: &mut impl Write) {
        self.agent = None;
        self.subscription = None;
        if let Some(host) = self.host.take() {
            let report = host.shutdown(std::time::Duration::from_secs(3)).await;
            // Shutdown reports what it completed, not a guarantee: work
            // still pending means deleting the run root would drop journals
            // the session still owns, so it is kept like `--root`.
            if work_remains(&report) {
                self.root = Some(self.root_path.clone());
                let _ = writeln!(
                    out,
                    "shutdown left {} task(s), status_quiet={} — keeping run root",
                    report.tasks_remaining, report.status_quiet
                );
            }
        }
        if self.keep || self.root.is_some() {
            let _ = writeln!(out, "kept run root: {}", self.root_path.display());
        }
    }

    /// The last tracked turn, required by turn-scoped steps.
    fn turn(&self, line: usize, what: &str) -> Result<TurnId, DevError> {
        self.last_turn.ok_or_else(|| DevError::Step {
            line,
            detail: format!("{what} needs a turn: submit a prompt first"),
        })
    }

    /// Fails closed when a step authorizes in-band without `--consent`:
    /// consent must come from the invoker's command line, never from the
    /// scenario file.
    fn require_consent(&self, line: usize, what: &str) -> Result<(), DevError> {
        if self.consent {
            Ok(())
        } else {
            Err(DevError::Step {
                line,
                detail: format!("{what} needs the invoker's consent: pass --consent"),
            })
        }
    }

    /// Session steps may not follow the session's inputs.
    fn require_pre_session(&self, line: usize) -> Result<(), DevError> {
        if self.agent.is_some() || self.host.is_some() {
            return Err(DevError::Step {
                line,
                detail: "provider, config, and session steps must come before \
                         the first session step"
                    .to_owned(),
            });
        }
        Ok(())
    }

    /// Resolves a scenario path inside the workspace, rejecting escapes
    /// — including through symlinks an adopted `--root` workspace may
    /// hold. The deepest existing ancestor is canonicalized and must sit
    /// under the canonical workspace; the not-yet-created tail is then
    /// appended syntactically.
    fn inside_workspace(&self, path: &Path) -> Result<PathBuf, String> {
        let escapes = path.is_absolute()
            || path.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            });
        if escapes {
            return Err(format!("path `{}` escapes the workspace", path.display()));
        }
        let target = self.workspace.join(path);
        let mut probe = target.as_path();
        let mut tail = Vec::new();
        while !probe.exists() {
            let Some(name) = probe.file_name() else {
                break;
            };
            tail.push(name.to_os_string());
            let Some(parent) = probe.parent() else {
                break;
            };
            probe = parent;
        }
        let resolved = probe
            .canonicalize()
            .map_err(|_| format!("path `{}` cannot resolve", target.display()))?;
        if !resolved.starts_with(&self.workspace_canonical) {
            return Err(format!("path `{}` escapes the workspace", path.display()));
        }
        Ok(tail.iter().rev().fold(resolved, |acc, name| acc.join(name)))
    }

    /// Resolves the selected session kind against the run workspace.
    fn session_ref(&self) -> Result<SessionRef, DevError> {
        let workspace = Workspace::new(self.workspace.clone()).map_err(|error| DevError::Step {
            line: 0,
            detail: format!("workspace invalid: {error}"),
        })?;
        Ok(match &self.session_kind {
            SessionKind::New(name) => SessionRef::New {
                workspace,
                name: name.clone().map(String::into_boxed_str),
            },
            SessionKind::Resume(key) => SessionRef::Resume {
                key: key.clone().into_boxed_str(),
                workspace,
            },
            SessionKind::Continue => SessionRef::Continue { workspace },
            SessionKind::Ephemeral => SessionRef::Ephemeral { workspace },
        })
    }

    /// Builds the user config text from scenario config lines and the provider.
    fn user_config(&self) -> String {
        // `ask` is the default: a scenario that wants patch or exec calls
        // declares `approval = "all"` in a config step under `--consent`,
        // like the product itself requires.
        let mut text = if self.config_text.is_empty() {
            "model = \"openai-responses/gpt-6\"\napproval = \"ask\"\n".to_owned()
        } else {
            self.config_text.join("\n")
        };
        if !self.plugins.is_empty() {
            let list = self
                .plugins
                .iter()
                .map(|name| format!("\"{name}\""))
                .collect::<Vec<_>>()
                .join(", ");
            // Root keys must precede every table declaration; prepend so an
            // arbitrary config step cannot hide `plugins` inside a table.
            text.insert_str(0, &format!("plugins = [{list}]\n"));
        }
        if let Some(fixture) = &self.provider_fixture {
            // The TOML serializer, not an escape table: quotes, newlines,
            // and control characters cannot produce malformed config.
            let rendered = toml_string(fixture.to_string_lossy().as_ref());
            let _ = writeln!(text, "\n[providers.scripted]\nfixture = {rendered}");
        }
        text
    }

    /// Installs one plugin source under the run data root and enables it.
    fn plugin_step(
        &mut self,
        wire: PluginWire,
        invalid: &impl Fn(String) -> DevError,
    ) -> Result<String, DevError> {
        let (name, source) = match wire {
            PluginWire::Bundled(name) => {
                if name != "devprobe" {
                    return Err(invalid(format!(
                        "unknown bundled plugin `{name}`: only `devprobe` is bundled"
                    )));
                }
                (name, DEVPROBE_STAR.to_owned())
            }
            PluginWire::File(wire) => {
                let PluginFileWire { name, file } = wire;
                let path = resolve(&self.scenario_dir, &file);
                let source = std::fs::read_to_string(&path).map_err(|source| DevError::Read {
                    path: path.display().to_string(),
                    source,
                })?;
                (name, source)
            }
        };
        // The name becomes one path component under the data root: reject
        // separators, anchors, and `..` before any filesystem I/O so a
        // scenario cannot write `plugin.star` outside its own run.
        let mut components = std::path::Path::new(&name).components();
        if !(components
            .next()
            .is_some_and(|part| matches!(part, std::path::Component::Normal(_)))
            && components.next().is_none())
        {
            return Err(invalid(format!(
                "plugin name `{name}` must be a single path component"
            )));
        }
        let dir = self.data_root.join("plugins").join(&name);
        std::fs::create_dir_all(&dir).map_err(|source| DevError::Write {
            path: dir.display().to_string(),
            source,
        })?;
        let target = dir.join("plugin.star");
        std::fs::write(&target, source).map_err(|source| DevError::Write {
            path: target.display().to_string(),
            source,
        })?;
        self.plugins.push(name.clone());
        Ok(name)
    }

    /// Validates a provider step and returns the fixture file path.
    fn provider_fixture(
        &self,
        wire: ProviderWire,
        invalid: &impl Fn(String) -> DevError,
    ) -> Result<PathBuf, DevError> {
        match (wire.replay, wire.script) {
            (Some(path), None) => {
                let file = resolve(&self.scenario_dir, &path);
                let bytes = std::fs::read(&file).map_err(|source| DevError::Read {
                    path: file.display().to_string(),
                    source,
                })?;
                Script::from_replay(&bytes)
                    .map_err(|error| invalid(format!("provider replay invalid: {error}")))?;
                Ok(file)
            }
            (None, Some(steps)) => {
                let mut text = String::new();
                for step in &steps {
                    text.push_str(step.as_str());
                    text.push('\n');
                }
                Script::from_replay(text.as_bytes())
                    .map_err(|error| invalid(format!("provider script invalid: {error}")))?;
                let file = self.root_path.join("provider-script.jsonl");
                std::fs::write(&file, text).map_err(|source| DevError::Write {
                    path: file.display().to_string(),
                    source,
                })?;
                Ok(file)
            }
            (Some(_), Some(_)) => Err(invalid(
                "`provider` takes exactly one of `replay` or `script`".to_owned(),
            )),
            (None, None) => Err(invalid("`provider` needs `replay` or `script`".to_owned())),
        }
    }

    /// Starts the host, opens the session, and subscribes to its updates.
    async fn ensure_started(&mut self, line: usize) -> Result<(), DevError> {
        if self.agent.is_some() {
            return Ok(());
        }
        let fail = |detail: String| DevError::Step { line, detail };
        if self.provider_fixture.is_none() && self.config_text.is_empty() {
            return Err(fail(
                "a `provider` or `config` step must precede the first session step".to_owned(),
            ));
        }
        let factory = crate::product();
        let user = self.user_config();
        let config = Config::load(
            ConfigProduct::Dalgon,
            &self.data_root,
            factory.defaults,
            Some(&user),
        )
        .map_err(|error| fail(format!("config rejected: {error}")))?;
        let product = (factory.build)(&BuildCx {
            data_root: self.data_root.clone(),
            config: &config,
        })
        .map_err(|error| fail(format!("product rejected: {error}")))?;
        let env = Env {
            vars: self.vars.clone(),
            cwd: self.workspace.clone(),
            sandbox_helper: self.helper.clone(),
        };
        let host = Host::start(product, config, env)
            .await
            .map_err(|error| fail(format!("host rejected: {error}")))?;
        let session = self.session_ref().map_err(|error| match error {
            DevError::Step { detail, .. } => DevError::Step { line, detail },
            other => other,
        })?;
        let agent = host
            .open(session, ClientId::new("dev"))
            .await
            .map_err(|error| fail(format!("session rejected: {error}")))?;
        self.subscription = Some(
            agent
                .subscribe(None)
                .map_err(|error| fail(format!("subscribe rejected: {error}")))?,
        );
        self.agent = Some(agent);
        self.host = Some(host);
        Ok(())
    }

    /// Submits one command to the live session.
    async fn submit(&mut self, command: Command, line: usize) -> Result<Reply, DevError> {
        self.ensure_started(line).await?;
        let agent = self.agent.as_ref().ok_or_else(|| DevError::Step {
            line,
            detail: "session closed".to_owned(),
        })?;
        let reply = agent
            .submit(command)
            .await
            .map_err(|error| DevError::Step {
                line,
                detail: format!("command rejected: {error}"),
            })?;
        if let Reply::Accepted { turn, .. } = &reply {
            self.last_turn = Some(*turn);
        }
        Ok(reply)
    }

    /// Awaits the next durable update or times out.
    ///
    /// Returns `Ok(None)` when the wait expires. Every received update feeds
    /// the run's turn and open-request tracking before the caller matches it.
    async fn poll(
        &mut self,
        timeout: Duration,
        line: usize,
    ) -> Result<Option<Arc<Update>>, DevError> {
        let fail = |detail: &str| DevError::Step {
            line,
            detail: detail.to_owned(),
        };
        self.ensure_started(line).await?;
        let subscription = self
            .subscription
            .as_mut()
            .ok_or_else(|| fail("update stream is closed"))?;
        let Ok(delivery) = tokio::time::timeout(timeout, subscription.next()).await else {
            return Ok(None);
        };
        match delivery {
            Some(Delivery::Update(update)) => {
                self.track(&update);
                Ok(Some(update))
            }
            Some(Delivery::Resync { .. }) => Err(fail("update cursor fell outside replay")),
            None => Err(fail("update stream ended")),
        }
    }

    /// Tracks turn and open-request state off every drained update.
    fn track(&mut self, update: &Update) {
        match &update.kind {
            UpdateKind::TurnStarted { turn, .. } => self.last_turn = Some(*turn),
            UpdateKind::RequestOpened(request) => self
                .open_requests
                .push_back((request.id, wire_type(&request.question))),
            UpdateKind::RequestResolved { id, .. } => {
                self.open_requests.retain(|(open, _)| *open != *id);
            }
            _ => {}
        }
    }

    /// Drains updates until one matches the expectation or the timeout ends.
    async fn expect_update(
        &mut self,
        spec: &UpdateSpec,
        line: usize,
        out: &mut impl Write,
    ) -> Result<(), DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        let timeout = spec
            .timeout_ms
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let deadline = deadline(timeout, line)?;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(fail(format!(
                    "no `{}` update within {}ms",
                    spec.kind,
                    timeout.as_millis()
                )));
            }
            let Some(update) = self.poll(remaining, line).await? else {
                return Err(fail(format!(
                    "no `{}` update within {}ms",
                    spec.kind,
                    timeout.as_millis()
                )));
            };
            let (kind, json) = update_shape(&update);
            let hit = kind == spec.kind
                && spec
                    .contains
                    .as_ref()
                    .is_none_or(|text| json.contains(text.as_str()))
                && spec
                    .not_contains
                    .as_ref()
                    .is_none_or(|text| !json.contains(text.as_str()));
            if hit {
                let _ = writeln!(out, "{line:>4}  update {kind}");
                return Ok(());
            }
        }
    }

    /// Drains until a request of the wanted kind opens, then answers it.
    async fn expect_request(
        &mut self,
        spec: &RequestSpec,
        line: usize,
        out: &mut impl Write,
    ) -> Result<(), DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        if spec.answer.is_some() {
            self.require_consent(line, "answering a request in-band")?;
        }
        let timeout = spec
            .timeout_ms
            .map_or(DEFAULT_TIMEOUT, Duration::from_millis);
        let deadline = deadline(timeout, line)?;
        loop {
            // A request an earlier expectation drained waits in the queue:
            // match the queue first so it is answered, not waited out.
            if let Some(index) = self
                .open_requests
                .iter()
                .position(|(_, kind)| *kind == spec.kind)
            {
                if let Some(answer) = &spec.answer {
                    // Only an answered request leaves the queue: observing
                    // must not strand it for a later `answer` step.
                    let Some((id, _)) = self.open_requests.remove(index) else {
                        continue;
                    };
                    let agent = self
                        .agent
                        .as_ref()
                        .ok_or_else(|| fail("no session".to_owned()))?;
                    agent
                        .answer(id, answer.clone())
                        .await
                        .map_err(|error| fail(format!("answer rejected: {error}")))?;
                }
                let _ = writeln!(out, "{line:>4}  request {}", spec.kind);
                return Ok(());
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(fail(format!(
                    "no `{}` request within {}ms",
                    spec.kind,
                    timeout.as_millis()
                )));
            }
            // `track` queues every opened request; `continue` re-checks the
            // queue so it is the single match path.
            let Some(_update) = self.poll(remaining, line).await? else {
                return Err(fail(format!(
                    "no `{}` request within {}ms",
                    spec.kind,
                    timeout.as_millis()
                )));
            };
        }
    }

    /// Counts one record kind across every journal under the data root.
    fn expect_journal(
        &mut self,
        spec: &JournalSpec,
        line: usize,
        out: &mut impl Write,
    ) -> Result<(), DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        let mut journals = Vec::new();
        find_journals(&self.data_root, &mut journals).map_err(|source| DevError::Read {
            path: self.data_root.display().to_string(),
            source,
        })?;
        if journals.is_empty() {
            return Err(fail(
                "no journal.jsonl under the run's data root (ephemeral sessions write none)"
                    .to_owned(),
            ));
        }
        let mut count = 0usize;
        for journal in &journals {
            for (index, line_text) in util::read_lines(journal)?.records.iter().enumerate() {
                let record = util::decode_line(journal, line_text, index + 1)?;
                if util::record_kind(&record) == spec.kind {
                    count += 1;
                }
            }
        }
        if count < spec.at_least {
            return Err(fail(format!(
                "found {count} `{}` records, expected at least {}",
                spec.kind, spec.at_least
            )));
        }
        let _ = writeln!(out, "{line:>4}  journal {} x{count}", spec.kind);
        Ok(())
    }

    /// Reads a workspace file and checks its contents.
    fn expect_file(
        &mut self,
        spec: &FileSpec,
        line: usize,
        out: &mut impl Write,
    ) -> Result<(), DevError> {
        let fail = |detail: String| DevError::Step { line, detail };
        let path = self.inside_workspace(&spec.path).map_err(fail)?;
        let text = std::fs::read_to_string(&path).map_err(|source| DevError::Read {
            path: path.display().to_string(),
            source,
        })?;
        if spec
            .contains
            .as_ref()
            .is_some_and(|want| !text.contains(want.as_str()))
        {
            return Err(fail(format!(
                "{} does not contain the expected text",
                spec.path.display()
            )));
        }
        if spec.equals.as_ref().is_some_and(|want| text != *want) {
            return Err(fail(format!(
                "{} does not equal the expected text",
                spec.path.display()
            )));
        }
        let _ = writeln!(out, "{line:>4}  file {}", spec.path.display());
        Ok(())
    }
}

impl Drop for RunCx {
    fn drop(&mut self) {
        if !self.keep && self.root.is_none() {
            let _ = std::fs::remove_dir_all(&self.root_path);
        }
    }
}

/// Serializes `value` and returns its `type` field.
fn wire_type<T: serde::Serialize>(value: &T) -> String {
    #[derive(Deserialize)]
    struct Probe {
        r#type: String,
    }
    sonic_rs::to_string(value)
        .ok()
        .and_then(|json| sonic_rs::from_str::<Probe>(&json).ok())
        .map_or_else(|| "?".to_owned(), |probe| probe.r#type)
}

/// Returns the update's wire kind and its serialized payload.
fn update_shape(update: &Update) -> (String, String) {
    let json = sonic_rs::to_string(&update.kind).unwrap_or_else(|_| "{}".to_owned());
    (wire_type(&update.kind), json)
}

/// One-line reply summaries for the step ledger.
fn reply_line(reply: &Reply) -> String {
    match reply {
        Reply::Accepted { turn, .. } => format!("accepted turn {}", turn.get()),
        Reply::Queued => "queued".to_owned(),
        Reply::Done(_) => "done".to_owned(),
        Reply::Choose { .. } => "choose".to_owned(),
        Reply::Front(_) => "front".to_owned(),
        Reply::Started(job) => format!("started job {job}"),
        _ => "reply".to_owned(),
    }
}

/// Resolves a scenario path against the scenario file's directory.
fn resolve(scenario_dir: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        scenario_dir.join(path)
    }
}

/// A config text that sets the root `approval` key authorizes in-band: the
/// root key only binds before the first table header, so scanning that
/// prefix is enough — a table-scoped `approval` is a provider's own setting.
/// The first config key that authorizes the product to act on its own:
/// a root `approval` mode, any `[providers.*]` wiring, or a credential or
/// endpoint setting at any level. A repository-controlled scenario could
/// point a credential-bearing request at an endpoint it owns, so every
/// such key needs the invoker's `--consent`. Decoding through `toml` keeps
/// quote tricks from hiding a key the strict loader would honor; text the
/// real decoder rejects fails later in `Config::load` anyway.
/// True when the shutdown report shows work still in flight — tasks the
/// host could not close inside its grace window, or a session that did
/// not reach quiet. A `true` verdict keeps the run root for `--root`
/// inspection instead of deleting the journals mid-flight.
fn work_remains(report: &dal_agent::ShutdownReport) -> bool {
    report.tasks_remaining > 0 || !report.status_quiet
}

/// The largest timeout a scenario may declare. A `timeout_ms` past one
/// day is a malformed scenario, and on platforms with a narrower clock
/// range the arithmetic would overflow outright.
#[expect(
    clippy::duration_suboptimal_units,
    reason = "Duration::from_hours is not yet const-stable (rust#140881)"
)]
const MAX_TIMEOUT: Duration = Duration::from_secs(24 * 60 * 60);

/// The deadline for one expectation: the declared timeout bounded by the
/// scenario limit, then the platform clock.
fn deadline(timeout: Duration, line: usize) -> Result<Instant, DevError> {
    if timeout > MAX_TIMEOUT {
        return Err(DevError::Step {
            line,
            detail: format!(
                "timeout_ms {} exceeds the {}s scenario limit",
                timeout.as_millis(),
                MAX_TIMEOUT.as_secs()
            ),
        });
    }
    Instant::now()
        .checked_add(timeout)
        .ok_or_else(|| DevError::Step {
            line,
            detail: format!(
                "timeout_ms {} exceeds the platform clock range",
                timeout.as_millis()
            ),
        })
}

/// One Rust string as one TOML basic-string literal. The document
/// serializer cannot emit a bare scalar, so a one-key map carries it.
#[expect(
    clippy::expect_used,
    reason = "serializing a one-key string map cannot fail"
)]
fn toml_string(value: &str) -> String {
    const KEY: &str = "v";
    let doc = toml::to_string(&toml::map::Map::from_iter([(
        KEY.to_owned(),
        toml::Value::String(value.to_owned()),
    )]))
    .expect("a string value always serializes");
    doc.trim_end()
        .strip_prefix("v = ")
        .unwrap_or("\"\"")
        .to_owned()
}

fn gated_config_key(text: &str) -> Option<String> {
    const GATED: &[&str] = &[
        "providers",
        "provider",
        "key_env",
        "api_key",
        "base_url",
        "token",
        "credential",
        "credentials",
        "secret",
        "auth",
        "bearer",
    ];
    fn scan(table: &toml::Table, root: bool) -> Option<String> {
        for (key, value) in table {
            if (root && key == "approval") || GATED.contains(&key.as_str()) {
                return Some(key.clone());
            }
            if let Some(inner) = value.as_table()
                && let Some(hit) = scan(inner, false)
            {
                return Some(hit);
            }
        }
        None
    }
    let table = toml::from_str::<toml::Table>(text).ok()?;
    scan(&table, true)
}

/// Collects every journal.jsonl under `dir`.
fn find_journals(dir: &Path, out: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            find_journals(&path, out)?;
        } else if entry.file_name() == "journal.jsonl" {
            out.push(path);
        }
    }
    Ok(())
}

/// Parses an approval-mode name.
fn approval_mode(text: &str) -> Result<ApprovalMode, String> {
    match text {
        "ask" => Ok(ApprovalMode::Ask),
        "edits" => Ok(ApprovalMode::Edits),
        "all" => Ok(ApprovalMode::All),
        other => Err(format!("unknown approval mode `{other}`")),
    }
}

/// Parses a thinking-level name.
fn thinking_level(text: &str) -> Result<ThinkingLevel, String> {
    match text {
        "off" => Ok(ThinkingLevel::Off),
        "minimal" => Ok(ThinkingLevel::Minimal),
        "low" => Ok(ThinkingLevel::Low),
        "medium" => Ok(ThinkingLevel::Medium),
        "high" => Ok(ThinkingLevel::High),
        "xhigh" => Ok(ThinkingLevel::Xhigh),
        "max" => Ok(ThinkingLevel::Max),
        other => Err(format!("unknown thinking level `{other}`")),
    }
}

// -- scenario step wire shapes ------------------------------------------------

/// The single-key step object on one scenario line.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StepWire {
    provider: Option<ProviderWire>,
    config: Option<ConfigWire>,
    plugin: Option<PluginWire>,
    session: Option<SessionWire>,
    write: Option<WriteWire>,
    prompt: Option<PromptWire>,
    steer: Option<String>,
    follow_up: Option<String>,
    run: Option<RunWire>,
    reload: Option<bool>,
    cancel: Option<String>,
    set_model: Option<String>,
    set_approval: Option<String>,
    set_thinking: Option<String>,
    compact: Option<CompactWire>,
    rename: Option<String>,
    export: Option<ExportWire>,
    answer: Option<AnswerWire>,
    expect: Option<ExpectWire>,
    sleep: Option<u64>,
    comment: Option<String>,
}

/// `{"provider": ...}` — a replay file or an inline scripted fixture.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ProviderWire {
    replay: Option<PathBuf>,
    script: Option<Vec<RawJson>>,
}

/// `{"config": ...}` — inline TOML text or a file path.
#[derive(Deserialize)]
#[serde(untagged)]
enum ConfigWire {
    Text(String),
    File(ConfigFileWire),
}

/// `{"config": {"file": f}}` — TOML text loaded from a scenario-relative path.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConfigFileWire {
    file: PathBuf,
}

/// `{"plugin": "devprobe"}` bundles the probe, or `{"plugin": {"name": n,
/// "file": f}}` copies a scenario-relative `.star` file into the data root.
#[derive(Deserialize)]
#[serde(untagged)]
enum PluginWire {
    Bundled(String),
    File(PluginFileWire),
}

/// The file form of a plugin step.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PluginFileWire {
    name: String,
    file: PathBuf,
}

/// `{"session": ...}` — a session shape or a named string.
#[derive(Deserialize)]
#[serde(untagged)]
enum SessionWire {
    Name(SessionName),
    Spec(SessionSpecWire),
}

/// The simple session kinds.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum SessionName {
    New,
    Ephemeral,
    Continue,
}

/// `{"session": {"new": ...}}` or `{"session": {"resume": key}}`.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SessionSpecWire {
    new: Option<NewSessionWire>,
    resume: Option<String>,
}

/// `{"new": true}` or `{"new": {"name": ...}}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum NewSessionWire {
    Flag(bool),
    Named(NewSessionNamedWire),
}

/// `{"new": {"name": ...}}` — a named fresh session.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSessionNamedWire {
    name: String,
}

/// `{"write": ...}` — a workspace-relative file.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteWire {
    path: PathBuf,
    text: String,
}

/// `{"prompt": ...}` — text or a spec with an expect gate.
#[derive(Deserialize)]
#[serde(untagged)]
enum PromptWire {
    Text(String),
    Spec(PromptSpecWire),
}

/// The full prompt spec.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PromptSpecWire {
    text: String,
    expect: Option<PromptExpectWire>,
}

/// A prompt's session-state gate.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum PromptExpectWire {
    Idle,
    Previous,
}

/// `{"run": ...}` — a command name or name+args.
#[derive(Deserialize)]
#[serde(untagged)]
enum RunWire {
    Name(String),
    Spec(RunSpecWire),
}

/// `{"run": {"name": n, "args": a}}` — a command with arguments.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RunSpecWire {
    name: String,
    args: Option<String>,
}

/// `{"compact": ...}` — a bare flag or a focus.
#[derive(Deserialize)]
#[serde(untagged)]
enum CompactWire {
    Flag(bool),
    Focus(CompactFocusWire),
}

/// `{"compact": {"focus": ...}}` — a focused compaction.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactFocusWire {
    focus: String,
}

/// `{"export": ...}` — an export target.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExportWire {
    path: Option<PathBuf>,
    format: ExportFormat,
}

/// `{"answer": ...}` — the oldest open request, or a targeted one.
#[derive(Deserialize)]
#[serde(untagged)]
enum AnswerWire {
    Simple(SimpleAnswer),
    Targeted(TargetedAnswerWire),
}

/// `{"answer": {"request": k, "answer": a}}` — an answer for a kind.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetedAnswerWire {
    request: String,
    answer: SimpleAnswer,
}

/// A plain answer: a named outcome or `{"value": <any JSON>}`.
#[derive(Deserialize)]
#[serde(untagged)]
enum SimpleAnswer {
    Name(AnswerName),
    Value(SimpleAnswerValueWire),
}

/// `{"value": <any JSON>}` — a typed answer payload.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SimpleAnswerValueWire {
    value: RawJson,
}

/// The named broker answers.
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum AnswerName {
    Approve,
    ApproveForSession,
    Decline,
    Cancel,
}

/// `{"expect": ...}` — exactly one of the five expectation kinds.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ExpectWire {
    update: Option<UpdateWire>,
    request: Option<RequestWire>,
    journal: Option<JournalWire>,
    file: Option<FileWire>,
    quiet_ms: Option<u64>,
}

/// `{"update": ...}` — a kind name or a full spec.
#[derive(Deserialize)]
#[serde(untagged)]
enum UpdateWire {
    Kind(String),
    Spec(UpdateSpecWire),
}

/// The full update expectation spec.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateSpecWire {
    kind: String,
    contains: Option<String>,
    not_contains: Option<String>,
    timeout_ms: Option<u64>,
}

/// `{"request": ...}` — a question kind or a spec with an answer.
#[derive(Deserialize)]
#[serde(untagged)]
enum RequestWire {
    Kind(String),
    Spec(RequestSpecWire),
}

/// The full request expectation spec.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RequestSpecWire {
    kind: String,
    answer: Option<SimpleAnswer>,
    timeout_ms: Option<u64>,
}

/// `{"journal": ...}` — a record kind or a spec with a count.
#[derive(Deserialize)]
#[serde(untagged)]
enum JournalWire {
    Kind(String),
    Spec(JournalSpecWire),
}

/// The full journal expectation spec.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct JournalSpecWire {
    kind: String,
    at_least: Option<usize>,
}

/// `{"file": ...}` — a workspace file expectation.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct FileWire {
    path: PathBuf,
    contains: Option<String>,
    equals: Option<String>,
}

impl StepWire {
    /// Converts the decoded wire into a step, requiring exactly one key.
    #[expect(
        clippy::too_many_lines,
        reason = "the key-dispatch table is the scenario vocabulary's index"
    )]
    fn decode(self, invalid: &impl Fn(String) -> DevError) -> Result<Step, DevError> {
        let mut keys = Vec::new();
        for (name, set) in [
            ("provider", self.provider.is_some()),
            ("config", self.config.is_some()),
            ("plugin", self.plugin.is_some()),
            ("session", self.session.is_some()),
            ("write", self.write.is_some()),
            ("prompt", self.prompt.is_some()),
            ("steer", self.steer.is_some()),
            ("follow_up", self.follow_up.is_some()),
            ("run", self.run.is_some()),
            ("reload", self.reload.is_some()),
            ("cancel", self.cancel.is_some()),
            ("set_model", self.set_model.is_some()),
            ("set_approval", self.set_approval.is_some()),
            ("set_thinking", self.set_thinking.is_some()),
            ("compact", self.compact.is_some()),
            ("rename", self.rename.is_some()),
            ("export", self.export.is_some()),
            ("answer", self.answer.is_some()),
            ("expect", self.expect.is_some()),
            ("sleep", self.sleep.is_some()),
            ("comment", self.comment.is_some()),
        ] {
            if set {
                keys.push(name);
            }
        }
        match keys.as_slice() {
            [] => return Err(invalid("empty step object".to_owned())),
            [_] => {}
            many => {
                return Err(invalid(format!(
                    "step holds {} keys ({}): pass exactly one",
                    many.len(),
                    many.join(", ")
                )));
            }
        }

        if let Some(wire) = self.provider {
            return Ok(Step::Provider(wire));
        }
        if let Some(wire) = self.config {
            return Ok(Step::Config(wire));
        }
        if let Some(wire) = self.plugin {
            return Ok(Step::Plugin(wire));
        }
        if let Some(wire) = self.session {
            let kind = match wire {
                SessionWire::Name(SessionName::New) => SessionKind::New(None),
                SessionWire::Name(SessionName::Ephemeral) => SessionKind::Ephemeral,
                SessionWire::Name(SessionName::Continue) => SessionKind::Continue,
                SessionWire::Spec(spec) => match (spec.new, spec.resume) {
                    (Some(NewSessionWire::Flag(false)), None) => {
                        return Err(invalid(
                            "`new` selects a session: pass `true` or a name".to_owned(),
                        ));
                    }
                    (Some(NewSessionWire::Flag(true)) | None, None) => SessionKind::New(None),
                    (Some(NewSessionWire::Named(wire)), None) => SessionKind::New(Some(wire.name)),
                    (None, Some(key)) => SessionKind::Resume(key),
                    (Some(_), Some(_)) => {
                        return Err(invalid(
                            "`session` takes one of `new` or `resume`".to_owned(),
                        ));
                    }
                },
            };
            return Ok(Step::Session(kind));
        }
        if let Some(wire) = self.write {
            return Ok(Step::Write {
                path: wire.path,
                text: wire.text,
            });
        }
        if let Some(wire) = self.prompt {
            let (text, gate) = match wire {
                PromptWire::Text(text) => (text, PromptGate::Idle),
                PromptWire::Spec(spec) => (
                    spec.text,
                    match spec.expect {
                        None | Some(PromptExpectWire::Idle) => PromptGate::Idle,
                        Some(PromptExpectWire::Previous) => PromptGate::Previous,
                    },
                ),
            };
            return Ok(Step::Prompt { text, gate });
        }
        if let Some(text) = self.steer {
            return Ok(Step::Steer(text));
        }
        if let Some(text) = self.follow_up {
            return Ok(Step::FollowUp(text));
        }
        if let Some(wire) = self.run {
            let (name, args) = match wire {
                RunWire::Name(name) => (name, String::new()),
                RunWire::Spec(spec) => (spec.name, spec.args.unwrap_or_default()),
            };
            return Ok(Step::Run { name, args });
        }
        if let Some(on) = self.reload {
            if !on {
                return Err(invalid(
                    "`reload` is only a trigger: pass `true`".to_owned(),
                ));
            }
            return Ok(Step::Reload);
        }
        if let Some(scope) = self.cancel {
            if scope != "turn" {
                return Err(invalid("`cancel` only accepts `turn`".to_owned()));
            }
            return Ok(Step::Cancel);
        }
        if let Some(id) = self.set_model {
            return Ok(Step::SetModel(id));
        }
        if let Some(mode) = self.set_approval {
            return Ok(Step::SetApproval(mode));
        }
        if let Some(level) = self.set_thinking {
            return Ok(Step::SetThinking(level));
        }
        if let Some(wire) = self.compact {
            return Ok(Step::Compact(match wire {
                CompactWire::Flag(false) => {
                    return Err(invalid(
                        "`compact` takes `true` or a focus object".to_owned(),
                    ));
                }
                CompactWire::Flag(true) => None,
                CompactWire::Focus(wire) => Some(wire.focus),
            }));
        }
        if let Some(name) = self.rename {
            return Ok(Step::Rename(name));
        }
        if let Some(wire) = self.export {
            return Ok(Step::Export {
                path: wire.path,
                format: wire.format,
            });
        }
        if let Some(wire) = self.answer {
            let (kind, simple) = match wire {
                AnswerWire::Simple(simple) => (None, simple),
                AnswerWire::Targeted(wire) => (Some(wire.request), wire.answer),
            };
            return Ok(Step::Answer {
                kind,
                answer: decode_answer(simple),
            });
        }
        if let Some(wire) = self.expect {
            return decode_expect(wire, invalid);
        }
        if let Some(ms) = self.sleep {
            return Ok(Step::Sleep(ms));
        }
        if let Some(text) = self.comment {
            return Ok(Step::Comment(text));
        }
        Err(invalid("step key could not be decoded".to_owned()))
    }
}

/// Decodes a `SimpleAnswer` into a broker `Answer`.
fn decode_answer(simple: SimpleAnswer) -> Answer {
    match simple {
        SimpleAnswer::Name(AnswerName::Approve) => Answer::Approve,
        SimpleAnswer::Name(AnswerName::ApproveForSession) => Answer::ApproveForSession,
        SimpleAnswer::Name(AnswerName::Decline) => Answer::Decline,
        SimpleAnswer::Name(AnswerName::Cancel) => Answer::Cancel,
        SimpleAnswer::Value(wire) => Answer::Value(wire.value),
    }
}

/// Decodes the `expect` object: exactly one expectation kind per step.
fn decode_expect(
    wire: ExpectWire,
    invalid: &impl Fn(String) -> DevError,
) -> Result<Step, DevError> {
    let mut keys = Vec::new();
    for (name, set) in [
        ("update", wire.update.is_some()),
        ("request", wire.request.is_some()),
        ("journal", wire.journal.is_some()),
        ("file", wire.file.is_some()),
        ("quiet_ms", wire.quiet_ms.is_some()),
    ] {
        if set {
            keys.push(name);
        }
    }
    match keys.as_slice() {
        [] => {
            return Err(invalid(
                "`expect` needs one of: update, request, journal, file, quiet_ms".to_owned(),
            ));
        }
        [_] => {}
        many => {
            return Err(invalid(format!(
                "`expect` holds {} keys ({}): pass exactly one",
                many.len(),
                many.join(", ")
            )));
        }
    }
    if let Some(update) = wire.update {
        let spec = match update {
            UpdateWire::Kind(kind) => UpdateSpec {
                kind,
                contains: None,
                not_contains: None,
                timeout_ms: None,
            },
            UpdateWire::Spec(spec) => UpdateSpec {
                kind: spec.kind,
                contains: spec.contains,
                not_contains: spec.not_contains,
                timeout_ms: spec.timeout_ms,
            },
        };
        return Ok(Step::ExpectUpdate(spec));
    }
    if let Some(request) = wire.request {
        let spec = match request {
            RequestWire::Kind(kind) => RequestSpec {
                kind,
                answer: None,
                timeout_ms: None,
            },
            RequestWire::Spec(spec) => RequestSpec {
                kind: spec.kind,
                answer: spec.answer.map(decode_answer),
                timeout_ms: spec.timeout_ms,
            },
        };
        return Ok(Step::ExpectRequest(spec));
    }
    if let Some(journal) = wire.journal {
        let spec = match journal {
            JournalWire::Kind(kind) => JournalSpec { kind, at_least: 1 },
            JournalWire::Spec(spec) => JournalSpec {
                kind: spec.kind,
                at_least: spec.at_least.unwrap_or(1),
            },
        };
        return Ok(Step::ExpectJournal(spec));
    }
    if let Some(file) = wire.file {
        return Ok(Step::ExpectFile(FileSpec {
            path: file.path,
            contains: file.contains,
            equals: file.equals,
        }));
    }
    if let Some(ms) = wire.quiet_ms {
        return Ok(Step::ExpectQuiet(ms));
    }
    Err(invalid("expectation could not be decoded".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::{gated_config_key, toml_string, work_remains};

    /// `dev run` consults the shutdown report before deleting the root:
    /// leftover tasks or a non-quiet session keep it. Reverting to the
    /// unconditional delete drops journals mid-flight.
    #[test]
    fn work_remains_follows_the_shutdown_report() {
        let base = dal_agent::ShutdownReport {
            sessions_closed: 1,
            status_quiet: true,
            tasks_remaining: 0,
        };
        assert!(!work_remains(&base));
        assert!(work_remains(&dal_agent::ShutdownReport {
            tasks_remaining: 1,
            ..base
        }));
        assert!(work_remains(&dal_agent::ShutdownReport {
            status_quiet: false,
            ..base
        }));
    }

    /// A fixture path with quotes, backslashes, or a newline must
    /// round-trip through the config TOML instead of corrupting it.
    /// Reverting to raw interpolation breaks the decode for each shape.
    #[test]
    fn toml_string_round_trips_hostile_paths() {
        for path in [
            "/plain/path",
            "C:\\Users\\a\\fixture.jsonl",
            "has \"a quote\" inside",
            "line1\nline2",
            "tab\there",
        ] {
            let doc = format!("fixture = {}", toml_string(path));
            let parsed: toml::Table = toml::from_str(&doc).expect(&doc);
            assert_eq!(
                parsed["fixture"].as_str(),
                Some(path),
                "round-trip failed for {path:?}"
            );
        }
    }

    /// The consent gate reads the decoded TOML, not the literal text: a
    /// provider credential key or a root `approval` gates in any quoting
    /// or table form. Reverting to the text scan misses the
    /// single-quoted and dotted-table shapes.
    #[test]
    fn gated_config_key_reads_the_decoded_toml() {
        for text in [
            "approval = \"all\"",
            "approval = 'all'",
            "[providers.test-api]\napi_key = \"x\"",
            "providers.scripted.base_url = \"http://x\"",
            "providers.test.key_env = \"X\"",
            "[providers.a.b]\ntoken = \"x\"",
        ] {
            assert!(gated_config_key(text).is_some(), "{text}");
        }
        for text in [
            "plugins = []",
            "model = \"openai/gpt-6-luna\"",
            "# approval = \"all\"",
            "not toml at all {{{",
            "",
        ] {
            assert!(gated_config_key(text).is_none(), "{text}");
        }
    }
}
