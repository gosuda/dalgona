// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Role and system inputs of orchestration children, through the real host.
//!
//! A root session whose scripted model calls the real `agents` tool starts
//! children through the real orchestration extension, the real scope, the
//! real `agents.start` service, and the real child session. The children run
//! on a probe model that records the exact system prompt and first user
//! message of every provider request, then forwards the request to the
//! scripted provider. Nothing here fakes a host service: the probe sits at
//! the provider boundary, where the model would read the child's inputs.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::grants::{GrantKey, GrantStore};
use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
    PromptOrder, PromptSection,
};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryKind, Expect, Family,
    JournalPart, ModelId, ModelRequest, ModelRoute, PageReq, Part, ServiceSet, SessionId,
    ThinkingLevel, Timestamp, UpdateKind, Workspace,
};
use dalgona_batteries::orchestration::{OrchestrationConfig, orchestration, parse_config};

type TestResult = Result<(), Box<dyn Error>>;

/// Long enough for a cold host and the children it starts.
const WAIT: Duration = Duration::from_secs(60);

const PROBE_MODEL: &str = "kit/probe";
/// A static extension prompt section every child must still render.
const SENTINEL: &str = "CHILD-INPUT-EXTENSION-SECTION";
const ROLE: &str = "reviewer";
const OTHER_ROLE: &str = "tester";
const SYSTEM: &str = "Be concise.";

const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

/// One provider request a child made, reduced to the bytes this suite reads.
#[derive(Clone, Debug)]
struct Seen {
    /// The system prompt the model receives.
    system: String,
    /// The first user message of the request: the child's own task prompt.
    prompt: String,
}

/// The model every child uses: it records each request, then forwards it to
/// the scripted provider, whose queue answers.
#[derive(Default)]
struct Probe {
    seen: Mutex<Vec<Seen>>,
}

impl Probe {
    fn seen(&self) -> Vec<Seen> {
        self.seen
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }
}

impl ModelHandler for Probe {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        Box::pin(async move {
            let prompt = request
                .context
                .iter()
                .find_map(|item| match item {
                    ContextItem::User { parts } => parts.iter().find_map(|part| match part {
                        Part::Text { text } => Some(text.to_string()),
                        _ => None,
                    }),
                    _ => None,
                })
                .unwrap_or_default();
            self.seen
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(Seen {
                    system: request.system.to_string(),
                    prompt,
                });
            let mut forwarded = request;
            forwarded.model = ModelRoute::Api {
                family: Family::Chat,
                model: "gpt-6-luna".into(),
            };
            cx.forward(forwarded, &[]).await
        })
    }
}

fn caps() -> Caps {
    Caps {
        context_window: Some(100_000),
        thinking: Box::new([ThinkingLevel::Off]),
        tool_use: true,
        image_input: false,
        custom_grammar: false,
    }
}

/// The scripted step that ends a turn with plain text. Whichever session
/// asks next takes it, so every later step is interchangeable.
fn text_step() -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"done"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

/// The scripted step that makes the root call the `agents` tool once. The
/// fixture holds one step per line, so the arguments are compacted first.
fn agents_call_step(args: &str) -> Result<String, Box<dyn Error>> {
    let args = sonic_rs::to_string(&sonic_rs::from_str::<sonic_rs::Value>(args)?)?;
    Ok(format!(
        r#"{{"kind":"events","events":[{{"type":"tool_call_started","id":"c-agents","name":"agents"}},{{"type":"tool_calls_done","calls":[{{"id":"c-agents","name":"agents","args":{{"kind":"parsed","value":{args}}}}}]}},{USAGE},{{"type":"stop","reason":"tool_use"}}]}}"#
    ))
}

struct Rig {
    _root: TestRoot,
    host: Host,
    agent: dal_agent::Agent,
    probe: Arc<Probe>,
}

/// Opens a real host with the real orchestration battery, the probe model,
/// and a sentinel prompt section, then prompts the root once so its scripted
/// model calls the `agents` tool with `agents_args`.
async fn rig(
    agents_args: &str,
    tune: impl FnOnce(&mut OrchestrationConfig),
) -> Result<(Rig, dal_agent::Subscription), Box<dyn Error>> {
    let root = TestRoot(
        std::env::temp_dir().join(format!("dalgona-child-inputs-{}", SessionId::new_v7())),
    );
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    let mut steps = vec![agents_call_step(agents_args)?];
    steps.extend((0..64).map(|_| text_step()));
    let fixture = data.join("script.jsonl");
    fs::write(&fixture, steps.join("\n"))?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\napproval = \"all\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&user))?;

    let mut settings = parse_config(None)?;
    // The goal owner would wake the root with continuation turns; the
    // children are what this suite observes.
    settings.goal.enabled = false;
    settings.data_root = Some(data.clone());
    tune(&mut settings);
    let built = orchestration(settings)?;
    GrantStore::new(data.clone())
        .grant(
            GrantKey::for_extension(&built.extension, &workspace)
                .ok_or("the orchestration extension has no code identity")?,
            ClientId::new("child-inputs"),
            Timestamp::now(),
        )
        .await?;

    let probe = Arc::new(Probe::default());
    let kit = ExtensionBuilder::new("kit", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse(PROBE_MODEL)?,
            caps: caps(),
            handler: Arc::clone(&probe) as Arc<dyn ModelHandler>,
            export: None,
        })
        .build()?;
    let sentinel = ExtensionBuilder::new("probeext", "0.1.0", ServiceSet::EMPTY)?
        .prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            SENTINEL.into(),
        ))
        .build()?;
    let product = Product {
        name: "dalgona",
        data_root: data,
        defaults: "",
        extensions: vec![built.extension, kit, sentinel],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace)?,
                name: None,
            },
            ClientId::new("child-inputs"),
        )
        .await?;
    let subscription = agent.subscribe(None)?;
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "start the children".into(),
            }],
        })
        .await?;
    Ok((
        Rig {
            _root: root,
            host,
            agent,
            probe,
        },
        subscription,
    ))
}

impl Rig {
    /// Waits until the probe has served a request whose first user message
    /// contains every marker.
    async fn served(&self, markers: &[&str]) -> TestResult {
        tokio::time::timeout(WAIT, async {
            loop {
                let seen = self.probe.seen();
                if markers
                    .iter()
                    .all(|marker| seen.iter().any(|request| request.prompt.contains(marker)))
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .map_err(|_| format!("no child request carried every marker in {markers:?}"))?;
        Ok(())
    }

    /// The system prompts of every request whose first user message contains
    /// `marker`: one per turn the child has taken.
    fn systems(&self, marker: &str) -> Vec<String> {
        self.probe
            .seen()
            .into_iter()
            .filter(|request| request.prompt.contains(marker))
            .map(|request| request.system)
            .collect()
    }

    /// Asserts the child of `marker` took at least one turn and ran every
    /// turn under exactly `expected` as its system prompt.
    fn assert_system(&self, marker: &str, expected: &str) {
        let systems = self.systems(marker);
        assert!(!systems.is_empty(), "no request carried {marker}");
        for system in systems {
            assert_eq!(system, expected, "system prompt of the {marker} child");
        }
    }

    async fn finish(self) -> TestResult {
        let session = self.agent.view(PageReq::default())?.session.id;
        self.host.close(session).await?;
        self.host.shutdown(Duration::from_secs(5)).await;
        Ok(())
    }
}

#[tokio::test]
async fn role_and_system_reach_the_children_the_agents_tool_starts() -> TestResult {
    let args = format!(
        r#"{{"action":"run","name":"inputs","steps":[
            {{"name":"review","prompt":"marker-review","model":"{PROBE_MODEL}","role":"{ROLE}","isolation":"shared"}},
            {{"name":"audit","prompt":"marker-audit","model":"{PROBE_MODEL}","system":"{SYSTEM}","isolation":"shared"}},
            {{"name":"plain","prompt":"marker-plain","model":"{PROBE_MODEL}","isolation":"shared"}},
            {{"name":"check","prompt":"marker-check {{{{item}}}}","items":["one","two"],"model":"{PROBE_MODEL}","role":"{OTHER_ROLE}","isolation":"shared"}}
        ]}}"#
    );
    let (rig, _updates) = rig(&args, |_| {}).await?;
    rig.served(&[
        "marker-review",
        "marker-audit",
        "marker-plain",
        "marker-check one",
        "marker-check two",
    ])
    .await?;

    // The plain child carries only the product base the extension sections
    // build; every other child is judged against it.
    let base = rig
        .systems("marker-plain")
        .into_iter()
        .next()
        .ok_or("the plain child made no request")?;
    assert!(
        base.contains(SENTINEL),
        "the extension section reaches a child: {base:?}"
    );
    rig.assert_system("marker-plain", &base);
    rig.assert_system("marker-review", &format!("{base}\n\n{ROLE}"));
    rig.assert_system("marker-audit", &format!("{SYSTEM}\n\n{base}"));
    rig.assert_system("marker-check one", &format!("{base}\n\n{OTHER_ROLE}"));
    rig.assert_system("marker-check two", &format!("{base}\n\n{OTHER_ROLE}"));
    assert!(
        rig.probe.seen().iter().all(|request| {
            ["review", "audit", "plain", "check"]
                .iter()
                .any(|step| request.prompt.contains(&format!("marker-{step}")))
        }),
        "no child outside the four steps ran"
    );
    rig.finish().await
}

#[tokio::test]
async fn saved_workflow_role_and_system_reach_the_children_through_the_real_host() -> TestResult {
    let table: toml::Value = toml::from_str(&format!(
        r#"
        [vet]
        steps = [
            {{name = "review", prompt = "marker-saved-review", model = "{PROBE_MODEL}", role = "{ROLE}", isolation = "shared"}},
            {{name = "audit", prompt = "marker-saved-audit", model = "{PROBE_MODEL}", system = "{SYSTEM}", isolation = "shared"}},
            {{name = "plain", prompt = "marker-saved-plain", model = "{PROBE_MODEL}", isolation = "shared"}}
        ]
    "#
    ))?;
    let args = r#"{"action":"run","workflow":"vet"}"#;
    let (rig, _updates) = rig(args, |settings| settings.workflows = Some(table)).await?;
    rig.served(&[
        "marker-saved-review",
        "marker-saved-audit",
        "marker-saved-plain",
    ])
    .await?;
    let base = rig
        .systems("marker-saved-plain")
        .into_iter()
        .next()
        .ok_or("the plain saved step made no request")?;
    assert!(base.contains(SENTINEL), "{base:?}");
    rig.assert_system("marker-saved-plain", &base);
    rig.assert_system("marker-saved-review", &format!("{base}\n\n{ROLE}"));
    rig.assert_system("marker-saved-audit", &format!("{SYSTEM}\n\n{base}"));
    rig.finish().await
}

#[tokio::test]
async fn a_step_with_role_and_system_starts_no_child_through_the_real_host() -> TestResult {
    let args = format!(
        r#"{{"action":"run","steps":[
            {{"name":"first","prompt":"marker-first","model":"{PROBE_MODEL}","role":"{ROLE}","isolation":"shared"}},
            {{"name":"second","prompt":"marker-second","model":"{PROBE_MODEL}","role":"{ROLE}","system":"{SYSTEM}","isolation":"shared"}}
        ]}}"#
    );
    let (rig, mut updates) = rig(&args, |_| {}).await?;
    tokio::time::timeout(WAIT, async {
        while let Some(delivery) = updates.next().await {
            if let Delivery::Update(update) = delivery
                && matches!(update.kind, UpdateKind::TurnEnded { .. })
            {
                return Ok(());
            }
        }
        Err("the root session closed before its turn ended")
    })
    .await
    .map_err(|_| "the root turn never ended")??;

    let view = rig.agent.view(PageReq::default())?;
    let result = view
        .entries
        .items
        .iter()
        .find_map(|entry| match &entry.kind {
            EntryKind::ToolResult {
                name, error, parts, ..
            } if &**name == "agents" => Some((*error, parts)),
            _ => None,
        })
        .ok_or("the root journaled no agents result")?;
    let text: String = result
        .1
        .iter()
        .filter_map(|part| match part {
            JournalPart::Text { text } => Some(&**text),
            _ => None,
        })
        .collect();
    assert!(result.0, "the conflicting call fails: {text}");
    assert!(
        text.contains("agents: step second: role and system cannot both be set."),
        "{text}"
    );
    assert!(
        rig.probe.seen().is_empty(),
        "a refused call starts no child, not even the valid first step: {:?}",
        rig.probe.seen()
    );
    rig.finish().await
}
