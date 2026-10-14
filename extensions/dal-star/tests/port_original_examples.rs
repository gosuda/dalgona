//! Real-host proof for the locate, session-log, and env-read examples.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Output, Part, Reply, ToolOutcomeView,
    UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

type TestResult<T> = Result<T, Box<dyn Error + Send + Sync>>;

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;
const LOCATE: &str = include_str!("../../../examples/plugins/locate/plugin.star");
const SESSION_LOG: &str = include_str!("../../../examples/plugins/session-log/plugin.star");
const ENV_READ: &str = include_str!("../../../examples/plugins/env-read/plugin.star");

fn tool_step(tool: &str, id: &str, args: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"{id}","name":"{tool}"}},{{"type":"tool_calls_done","calls":[{{"id":"{id}","name":"{tool}","args":{{"kind":"parsed","value":{args}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}
"#
    )
}

fn text_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"done"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}
"#
    )
}

struct Fixture {
    root: tempfile::TempDir,
}

impl Fixture {
    fn new(plugin: &str, source: &str, script: &str) -> TestResult<Self> {
        let root = tempfile::tempdir()?;
        let plugin_dir = root.path().join("data/plugins").join(plugin);
        std::fs::create_dir_all(&plugin_dir)?;
        std::fs::create_dir_all(root.path().join("workspace"))?;
        std::fs::write(plugin_dir.join("plugin.star"), source)?;
        std::fs::write(root.path().join("data/scripted.jsonl"), script)?;
        Ok(Self { root })
    }

    fn data(&self) -> std::path::PathBuf {
        self.root.path().join("data")
    }

    fn workspace(&self) -> std::path::PathBuf {
        self.root.path().join("workspace")
    }

    async fn start(
        &self,
        configs: BTreeMap<String, String>,
        vars: &[(&str, &str)],
    ) -> TestResult<Host> {
        let data = self.data();
        let roots = LoadRoots {
            data_root: data.clone(),
            bundled: Vec::new(),
        };
        let plugins = PluginsConfig {
            enabled: Vec::new(),
            limits: dal_core::PluginLimits::default(),
            configs,
        };
        let mut extensions = vec![dal_tools::extension(dal_tools::ToolsConfig::default())?];
        extensions.extend(PluginSystem::new(load(&roots, &plugins)?, roots, plugins).extensions()?);
        let user = format!(
            "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
            data.join("scripted.jsonl").to_string_lossy()
        );
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;
        let product = Product {
            name: "dal",
            data_root: data,
            defaults: "",
            extensions,
            bundled: Vec::new(),
        };
        let mut env_vars =
            BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]);
        env_vars.extend(
            vars.iter()
                .map(|(key, value)| (OsString::from(key), OsString::from(value))),
        );
        let env = Env {
            vars: env_vars,
            cwd: self.workspace(),
            sandbox_helper: None,
        };
        Ok(Host::start(product, config, env).await?)
    }

    async fn open(&self, host: &Host, name: &str) -> TestResult<Agent> {
        Ok(host
            .open(
                SessionRef::New {
                    workspace: Workspace::new(self.workspace())?,
                    name: Some(name.into()),
                },
                ClientId::new(name),
            )
            .await?)
    }
}

async fn run_turn(agent: &Agent) -> TestResult<Vec<ToolOutcomeView>> {
    let mut updates = agent.subscribe(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "go".into() }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }
    let mut outcomes = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(30), updates.next())
            .await
            .map_err(|_| std::io::Error::other("turn timed out"))?
            .ok_or_else(|| std::io::Error::other("subscription closed"))?;
        let Delivery::Update(update) = delivery else {
            continue;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                agent.answer(request.id, Answer::Approve).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } => outcomes.push(outcome.clone()),
            UpdateKind::TurnEnded { .. } => break,
            _ => {}
        }
    }
    Ok(outcomes)
}

async fn run_command(agent: &Agent, name: &str, args: &str) -> TestResult<Reply> {
    let mut updates = agent.subscribe(None)?;
    let command = agent.submit(Command::Run {
        name: name.into(),
        args: args.into(),
        expected: None,
    });
    tokio::pin!(command);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            tokio::select! {
                biased;
                delivery = updates.next() => {
                    let delivery = delivery
                        .ok_or_else(|| std::io::Error::other("command subscription closed"))?;
                    if let Delivery::Update(update) = delivery
                        && let UpdateKind::RequestOpened(request) = &update.kind
                    {
                        agent.answer(request.id, Answer::Approve).await?;
                    }
                }
                result = &mut command => break Ok(result?),
            }
        }
    })
    .await
    .map_err(|_| std::io::Error::other("command timed out"))?
}

async fn command_text(agent: &Agent, name: &str, args: &str) -> TestResult<String> {
    match run_command(agent, name, args).await? {
        Reply::Done(Output::Text(text)) => Ok(text.into()),
        other => Err(std::io::Error::other(format!("unexpected {name} reply: {other:?}")).into()),
    }
}

fn texts(outcomes: &[ToolOutcomeView]) -> Vec<(bool, &str)> {
    outcomes
        .iter()
        .map(|outcome| (outcome.is_error, outcome.text.as_ref()))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn locate_reads_a_path_searches_a_pattern_and_rejects_neither() -> TestResult<()> {
    let tool = "locate__lookup";
    let script = [
        tool_step(tool, "read", r#"{"path":"notes.txt"}"#),
        tool_step(tool, "search", r#"{"pattern":"needle"}"#),
        tool_step(
            tool,
            "both",
            r#"{"pattern":"absent-text","path":"notes.txt"}"#,
        ),
        tool_step(tool, "neither", "{}"),
        text_step(),
    ]
    .concat();
    let fx = Fixture::new("locate", LOCATE, &script)?;
    std::fs::write(
        fx.workspace().join("notes.txt"),
        "first line\nthe needle line\n",
    )?;
    let host = fx.start(BTreeMap::new(), &[]).await?;
    let agent = fx.open(&host, "locate-session").await?;

    let outcomes = run_turn(&agent).await?;
    let results = texts(&outcomes);
    assert_eq!(results.len(), 4, "every scripted call settles: {results:?}");
    assert!(
        !results[0].0 && results[0].1.contains("the needle line"),
        "read returns the file text: {:?}",
        results[0]
    );
    assert!(!results[1].0, "search succeeds: {}", results[1].1);
    assert!(
        results[1].1.contains("notes.txt"),
        "search names the file: {}",
        results[1].1
    );
    assert!(
        results[1].1.contains("the needle line"),
        "search shows the matched line: {}",
        results[1].1
    );
    assert!(
        !results[2].0 && results[2].1.contains("first line"),
        "a path wins over a pattern: {}",
        results[2].1
    );
    assert_eq!(
        results[3],
        (
            true,
            "locate: give a pattern to search for, or a path to read"
        )
    );
    assert!(
        command_text(&agent, "locate:locate", "needle")
            .await?
            .contains("notes.txt"),
        "the positional command argument is the pattern"
    );
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn session_log_keeps_the_newest_fifty_markers_across_reopen() -> TestResult<()> {
    let script = std::iter::repeat_with(text_step)
        .take(51)
        .collect::<String>();
    let fx = Fixture::new("session-log", SESSION_LOG, &script)?;
    let host = fx.start(BTreeMap::new(), &[]).await?;
    let agent = fx.open(&host, "session-log-session").await?;
    let session = agent.session();

    assert!(
        run_turn(&agent).await?.is_empty(),
        "a text turn calls no tool"
    );
    assert_eq!(
        command_text(&agent, "session-log:log", "").await?,
        "- turn",
        "a new session's start precedes any client that can grant state"
    );
    for _ in 1..51 {
        run_turn(&agent).await?;
    }
    let log = command_text(&agent, "session-log:log", "").await?;
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(
        lines.len(),
        50,
        "51 turns keep the newest 50 markers: {log}"
    );
    assert!(lines.iter().all(|line| *line == "- turn"), "{log}");
    host.shutdown(Duration::from_secs(2)).await;
    drop(agent);

    let reopened_host = fx.start(BTreeMap::new(), &[]).await?;
    let reopened = reopened_host
        .open(
            SessionRef::Resume {
                key: session.to_string().into(),
                workspace: Workspace::new(fx.workspace())?,
            },
            ClientId::new("session-log-reopen"),
        )
        .await?;
    let log = command_text(&reopened, "session-log:log", "").await?;
    let lines: Vec<&str> = log.lines().collect();
    assert_eq!(lines.len(), 50, "the reopened log stays capped: {log}");
    assert_eq!(
        lines.last(),
        Some(&"- start"),
        "reopening records a start marker: {log}"
    );
    assert!(lines[..49].iter().all(|line| *line == "- turn"), "{log}");
    reopened_host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn session_log_hooks_stay_quiet_in_an_ephemeral_session() -> TestResult<()> {
    let fx = Fixture::new("session-log", SESSION_LOG, &text_step())?;
    let host = fx.start(BTreeMap::new(), &[]).await?;
    let agent = host
        .open(
            SessionRef::Ephemeral {
                workspace: Workspace::new(fx.workspace())?,
            },
            ClientId::new("session-log-ephemeral"),
        )
        .await?;
    assert!(
        run_turn(&agent).await?.is_empty(),
        "the turn ends without hook failures"
    );
    let reply = run_command(&agent, "session-log:log", "").await;
    assert!(
        format!("{reply:?}").contains("state is unavailable"),
        "the reader reports unavailable state: {reply:?}"
    );
    host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn env_read_prefers_the_argument_then_the_setting_and_names_unset_keys() -> TestResult<()> {
    let tool = "env-read__read";
    let script = [
        tool_step(tool, "named", r#"{"name":"DAL_EXAMPLE_OTHER"}"#),
        tool_step(tool, "configured", "{}"),
        tool_step(tool, "unset", r#"{"name":"DAL_EXAMPLE_ABSENT"}"#),
        text_step(),
    ]
    .concat();
    let fx = Fixture::new("env-read", ENV_READ, &script)?;
    let configs = BTreeMap::from([(
        String::from("env-read"),
        String::from(r#"{"key":"DAL_EXAMPLE_TOKEN"}"#),
    )]);
    let vars = [
        ("DAL_EXAMPLE_TOKEN", "token-value"),
        ("DAL_EXAMPLE_OTHER", "other-value"),
    ];
    let host = fx.start(configs, &vars).await?;
    let agent = fx.open(&host, "env-read-session").await?;

    let outcomes = run_turn(&agent).await?;
    assert_eq!(
        texts(&outcomes),
        [
            (false, "other-value"),
            (false, "token-value"),
            (true, "env-read: DAL_EXAMPLE_ABSENT is not set"),
        ]
    );
    host.shutdown(Duration::from_secs(2)).await;

    let unconfigured_host = fx.start(BTreeMap::new(), &vars).await?;
    let unconfigured = fx.open(&unconfigured_host, "env-read-unconfigured").await?;
    let reply = run_command(&unconfigured, "env-read:env", "").await;
    assert!(
        format!("{reply:?}").contains("env-read: give a name, or set key in [plugin.env-read]"),
        "no name and no setting asks for one: {reply:?}"
    );
    unconfigured_host.shutdown(Duration::from_secs(2)).await;
    Ok(())
}
