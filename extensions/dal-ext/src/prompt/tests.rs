use super::*;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use dal_agent::ext::prompt::SECTION_SEPARATOR;
use dal_agent::ext::tool::{ArgError, RawValue, Tool, ToolCall, ToolCx, ToolOutcome, ToolOutput};
use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, ModelCx, ModelError, ModelHandler,
    ModelRecord, PromptOrder, PromptSection,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, Expect, ModelId, ModelInfo, ModelRequest, Name,
    Part, RawJson, ServiceSet, Stop, ThinkingLevel, ToolClass, ToolSpec, UpdateKind, Visibility,
    Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};

use crate::skills::{BodyInput, SkillRegistration, SkillRegistry, validate_registration};

fn input<'a>(
    tool_lines: &'a [Box<str>],
    plugin_sections: &'a [(Box<str>, Box<str>)],
    environment: &'a [(Box<str>, Box<str>)],
    letter_fallback: &'a [Box<str>],
) -> PromptInput<'a> {
    PromptInput {
        product_name: "dal",
        product_version: "0.1",
        tool_lines,
        skills: Some("skill body\nkept as supplied"),
        rules: Some("rule body"),
        rulebook: Some("rulebook body"),
        plugin_sections,
        environment,
        instructions: Some("# Instructions from /project/AGENTS.md\nDo this"),
        system_md: Some("SYSTEM content"),
        letter_fallback,
        docs_line: None,
    }
}

#[test]
fn composition_uses_order_and_sorted_plugin_names() {
    let plugins = [
        (Box::from("zeta"), Box::from("zeta body")),
        (Box::from("alpha"), Box::from("alpha body")),
    ];
    let environment = [(Box::from("cwd"), Box::from("/work"))];
    let tools = [Box::from("read <path>"), Box::from("search <query>")];
    let fallbacks = [Box::from("fallback description")];
    let prompt = build(&input(&tools, &plugins, &environment, &fallbacks));

    let order = [
        "You are dal 0.1",
        D2_PREFERENCE_LINE,
        "- read <path>",
        "<skills>",
        "<rules>",
        "<rulebook>",
        "plugin 'alpha':",
        "plugin 'zeta':",
        "<environment>",
        "<instructions>",
        "<letter-fallback>",
        DEFAULT_DOCS_LINE,
    ];
    let mut previous = 0;
    for marker in order {
        let next = prompt[previous..]
            .find(marker)
            .expect("expected composition marker")
            + previous;
        assert!(next >= previous);
        previous = next + marker.len();
    }
    assert!(prompt.contains("skill body\nkept as supplied"));
    assert!(prompt.contains("<instructions>\n# Instructions from /project/AGENTS.md\nDo this\n\nSYSTEM content\n</instructions>"));
}

#[test]
fn rendered_instructions_keep_one_separator_before_system_text() {
    let files = [instructions::InstructionFile {
        path: "/project/AGENTS.md".into(),
        content: "Keep supplied content\n".into(),
        truncated: false,
    }];
    let rendered = instructions::render(&files).unwrap();
    let mut sections = input(&[], &[], &[], &[]);
    sections.instructions = Some(&rendered);
    let prompt = build(&sections);
    assert!(prompt.contains(
        "<instructions>\n# Instructions from /project/AGENTS.md\nKeep supplied content\n\nSYSTEM content\n</instructions>"
    ));
    for (body, system, joined) in [
        ("Keep\n", "SYSTEM content", "Keep\n\nSYSTEM content"),
        ("Keep\n\n\n", "SYSTEM content", "Keep\n\n\nSYSTEM content"),
        ("Keep", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
        ("Keep\n", "\nSYSTEM content", "Keep\n\nSYSTEM content"),
    ] {
        sections.instructions = Some(body);
        sections.system_md = Some(system);
        assert!(build(&sections).contains(joined));
    }
}

#[test]
fn prefix_ignores_suffix_inputs_and_matches_emitted_prefix() {
    let plugins = [(Box::from("letters"), Box::from("plugin body"))];
    let first_environment = [(Box::from("cwd"), Box::from("/one"))];
    let second_environment = [(Box::from("cwd"), Box::from("/two"))];
    let tools = [Box::from("read <path>"), Box::from("search <query>")];
    let first_fallbacks = [Box::from("fallback description")];
    let second_fallbacks = [Box::from("different fallback")];
    let first = input(&tools, &plugins, &first_environment, &first_fallbacks);
    let mut second = input(&tools, &plugins, &second_environment, &second_fallbacks);
    second.instructions = Some("changed instructions");
    second.system_md = Some("changed system text");
    second.docs_line = Some("changed docs line");

    let first_prompt = build(&first);
    let second_prompt = build(&second);
    let prefix_len = prefix_bytes(&first);
    assert_eq!(prefix_len, prefix_bytes(&second));
    assert_eq!(
        &first_prompt.as_bytes()[..prefix_len],
        &second_prompt.as_bytes()[..prefix_len]
    );
    assert!(first_prompt[..prefix_len].ends_with("</plugins>\n"));
    assert_ne!(first_prompt, second_prompt);
}

#[test]
fn empty_sections_are_omitted_and_docs_can_be_replaced() {
    let no_plugins: [(Box<str>, Box<str>); 0] = [];
    let no_environment: [(Box<str>, Box<str>); 0] = [];
    let no_tools: [Box<str>; 0] = [];
    let empty_fallback = [Box::from("")];
    let mut empty = input(&no_tools, &no_plugins, &no_environment, &empty_fallback);
    empty.skills = Some("");
    empty.rules = None;
    empty.rulebook = Some("");
    empty.instructions = Some("");
    empty.system_md = None;
    empty.docs_line = Some("custom docs line");

    let prompt = build(&empty);
    for omitted in [
        "<tools>",
        "<skills>",
        "<rules>",
        "<rulebook>",
        "<plugins>",
        "<environment>",
        "<instructions>",
        "<letter-fallback>",
        DEFAULT_DOCS_LINE,
    ] {
        assert!(!prompt.contains(omitted));
    }
    assert!(prompt.ends_with("custom docs line\n"));
}

#[test]
fn extension_registers_one_section_per_part_at_its_documented_order() {
    let built = extension().expect("prompt extension builds");
    assert_eq!(built.name(), "prompt");
    let orders: Vec<PromptOrder> = built
        .prompt_sections()
        .iter()
        .map(PromptSection::order)
        .collect();
    assert_eq!(
        orders,
        [
            PromptOrder::Identity,
            PromptOrder::Tools,
            PromptOrder::Instructions,
            PromptOrder::DocsLine,
        ]
    );
    assert_eq!(
        built.prompt_section().map(PromptSection::order),
        Some(PromptOrder::Identity),
        "the singular accessor still reports the first section"
    );
    assert!(built.schemes().is_empty());
}

/// Fixture text for the sections other owners contribute.
const D2_SECTION: &str = "D2-SECTION";
const RULES_SECTION: &str = "RULES-SECTION";
const PLUGINS_SECTION: &str = "PLUGINS-SECTION";
const ENVIRONMENT_SECTION: &str = "ENVIRONMENT-SECTION";
const FALLBACK_SECTION: &str = "FALLBACK-SECTION";

/// What `build` emits for the parts the `prompt` extension owns, given the
/// host's tool lines and no docs line.
fn composed(tool_lines: &[Box<str>]) -> String {
    let no_pairs: [(Box<str>, Box<str>); 0] = [];
    let no_text: [Box<str>; 0] = [];
    build(&PromptInput {
        product_name: "dal",
        product_version: env!("CARGO_PKG_VERSION"),
        tool_lines,
        skills: None,
        rules: None,
        rulebook: None,
        plugin_sections: &no_pairs,
        environment: &no_pairs,
        instructions: None,
        system_md: None,
        letter_fallback: &no_text,
        docs_line: Some(""),
    })
}

/// One extension whose only prompt section is fixed text at `order`.
fn fixed(name: &str, order: PromptOrder, text: &str) -> Extension {
    ExtensionBuilder::new(name, "0.1.0", ServiceSet::EMPTY)
        .expect("fixture builder")
        .prompt_section(PromptSection::static_text(order, text.into()))
        .build()
        .expect("fixture extension")
}

/// A model-visible tool with a fixed description; no turn here calls it.
struct ProbeTool {
    name: Name,
    spec: Arc<ToolSpec>,
}

impl ProbeTool {
    fn new() -> Self {
        let name = Name::parse("probe_tool").expect("tool name");
        let spec = Arc::new(ToolSpec {
            name: name.clone(),
            description: "Probe tool.".into(),
            parameters: RawJson::parse(r#"{"type":"object","properties":{}}"#)
                .expect("schema json"),
            grammar: None,
        });
        Self { name, spec }
    }
}

impl Tool for ProbeTool {
    fn name(&self) -> &Name {
        &self.name
    }

    fn spec(&self, _model: &ModelInfo) -> Arc<ToolSpec> {
        Arc::clone(&self.spec)
    }

    fn classify(&self, _args: &RawValue, _workspace: &Workspace) -> Result<ToolClass, ArgError> {
        Ok(ToolClass::Read)
    }

    fn run<'a>(&'a self, _call: ToolCall, _cx: ToolCx<'a>) -> BoxFuture<'a, ToolOutcome> {
        let outcome = ToolOutcome::Ok(Box::new(ToolOutput::from_text("unused")));
        Box::pin(async move { outcome })
    }
}

fn tool_probe() -> Extension {
    ExtensionBuilder::new("tool-probe", "0.1.0", ServiceSet::EMPTY)
        .expect("tool fixture builder")
        .tool(Arc::new(ProbeTool::new()), Visibility::Model)
        .build()
        .expect("tool fixture extension")
}

/// The real skills extension over one loaded skill.
fn skills_probe() -> Extension {
    let skill = validate_registration(
        "p",
        &Path::new("/plugins").join("p"),
        SkillRegistration {
            name: "find-anything",
            description: "Pick a search surface.",
            path: Path::new("SKILL.md"),
            letter2image: false,
        },
        BodyInput::Bytes("# FIND-BODY\nuse find\n".as_bytes()),
    )
    .expect("skill registration");
    let registry = SkillRegistry::merge(&[("p", vec![skill])])
        .map_err(|conflict| conflict.to_string())
        .expect("skill registry");
    crate::skills::extension(Arc::new(registry)).expect("skills extension")
}

/// Records the system prompt of every request, then ends the turn.
struct Recorder(Arc<Mutex<Vec<String>>>);

impl ModelHandler for Recorder {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request.system.to_string());
        let events: Vec<Result<StreamEvent, ProviderError>> = vec![
            Ok(StreamEvent::TextDelta {
                text: "done".into(),
            }),
            Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
            Ok(StreamEvent::Usage {
                usage: dal_core::Usage {
                    input_tokens: 1,
                    cached_input_tokens: 0,
                    output_tokens: 1,
                    reasoning_tokens: None,
                    cache_write_tokens: 0,
                    cost_usd: None,
                    premium_requests: None,
                },
            }),
            Ok(StreamEvent::Stop {
                reason: StopReason::EndTurn,
            }),
        ];
        let stream = EventStream::new(futures::stream::iter(events), || {});
        Box::pin(async move { Ok(stream) })
    }
}

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        let dir = std::env::temp_dir().join(format!(
            "dal-prompt-order-{}-{}",
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

/// A real host and one open session. Fields drop in order, so the host
/// releases the scratch directory before it is removed.
struct Rig {
    agent: Agent,
    _host: Host,
    _scratch: Scratch,
}

async fn start_rig(mut extensions: Vec<Extension>, systems: &Arc<Mutex<Vec<String>>>) -> Rig {
    let scratch = Scratch::new();
    let data = scratch.0.join("data");
    let workspace_dir = scratch.0.join("workspace");
    std::fs::create_dir_all(&data).expect("data directory");
    std::fs::create_dir_all(&workspace_dir).expect("workspace directory");
    let model = ExtensionBuilder::new("fixture-model", "0.1.0", ServiceSet::EMPTY)
        .expect("model fixture builder")
        .model(ModelRecord {
            id: ModelId::parse("fixture/end").expect("model id"),
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: true,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Recorder(Arc::clone(systems))),
            export: None,
        })
        .build()
        .expect("model fixture extension");
    extensions.push(model);
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )
    .expect("config");
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: workspace_dir.clone(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(workspace_dir).expect("workspace"),
                name: None,
            },
            ClientId::new("prompt-order"),
        )
        .await
        .expect("session");
    Rig {
        agent,
        _host: host,
        _scratch: scratch,
    }
}

async fn turn_ends(subscription: &mut Subscription) {
    let ended = async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            let UpdateKind::TurnEnded { stop, .. } = &update.kind else {
                continue;
            };
            assert_eq!(*stop, Stop::EndTurn, "the fixture turn ends cleanly");
            return;
        }
        panic!("subscription closed before the turn ended");
    };
    tokio::time::timeout(Duration::from_secs(20), ended)
        .await
        .expect("the turn ended in time");
}

/// Runs one prompt through a real host over `extensions` and returns the
/// system prompt the model received, as the real driver and assembler built it.
async fn assembled(extensions: Vec<Extension>) -> String {
    let systems = Arc::new(Mutex::new(Vec::new()));
    let rig = start_rig(extensions, &systems).await;
    let mut subscription = rig.agent.subscribe(None).expect("subscription");
    rig.agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: "go".into() }],
        })
        .await
        .expect("prompt accepted");
    turn_ends(&mut subscription).await;
    let seen = std::mem::take(&mut *systems.lock().unwrap_or_else(PoisonError::into_inner));
    match <[String; 1]>::try_from(seen) {
        Ok([system]) => system,
        Err(seen) => panic!("exactly one request expected, saw {seen:?}"),
    }
}

/// Every other owner's section, deliberately out of slot order.
fn other_owners() -> Vec<Extension> {
    vec![
        fixed(
            "environment-probe",
            PromptOrder::Environment,
            ENVIRONMENT_SECTION,
        ),
        fixed("fallback-probe", PromptOrder::Fallback, FALLBACK_SECTION),
        fixed("plugin-probe", PromptOrder::Plugins, PLUGINS_SECTION),
        fixed("rules-probe", PromptOrder::Rules, RULES_SECTION),
        fixed("d2-probe", PromptOrder::D2, D2_SECTION),
        tool_probe(),
        skills_probe(),
    ]
}

/// The real `prompt` extension after every other owner in the catalog.
fn catalog_prompt_last() -> Vec<Extension> {
    let mut catalog = other_owners();
    catalog.push(extension().expect("prompt extension builds"));
    catalog
}

/// The real `prompt` extension before every other owner in the catalog.
fn catalog_prompt_first() -> Vec<Extension> {
    let mut catalog = other_owners();
    catalog.insert(0, extension().expect("prompt extension builds"));
    catalog
}

#[tokio::test]
async fn assembled_prompt_orders_owned_sections_around_other_owners() {
    let system = assembled(catalog_prompt_last()).await;
    let head = composed(&[]);
    let tool_line: Box<str> = Box::from("probe_tool: Probe tool.");
    let with_tools = composed(&[tool_line]);
    let tools = with_tools
        .strip_prefix(head.as_str())
        .expect("the tools block follows the identity paragraph");
    let docs = format!("{DEFAULT_DOCS_LINE}\n");

    let opening = [head.as_str(), D2_SECTION, tools, "# Skills"].join(SECTION_SEPARATOR);
    assert!(
        system.starts_with(&opening),
        "identity, D2, tools, then skills: {system:?}"
    );
    let mut previous = system.find("# Skills").expect("skills section");
    for marker in [
        RULES_SECTION,
        PLUGINS_SECTION,
        ENVIRONMENT_SECTION,
        FALLBACK_SECTION,
        DEFAULT_DOCS_LINE,
    ] {
        let next = system
            .find(marker)
            .unwrap_or_else(|| panic!("{marker:?} missing from {system:?}"));
        assert!(next > previous, "{marker:?} out of order in {system:?}");
        previous = next;
    }
    let closing = [FALLBACK_SECTION, docs.as_str()].join(SECTION_SEPARATOR);
    assert!(
        system.ends_with(&closing),
        "the docs line closes the prompt: {system:?}"
    );
    assert_eq!(
        system,
        assembled(catalog_prompt_first()).await,
        "the prompt extension's catalog position moves no section"
    );
}

/// The scratch session has no instruction file and no `SYSTEM.md`, so the
/// instructions block is absent however the driver resolves them.
#[tokio::test]
async fn absent_tools_and_instructions_leave_no_section_or_separator() {
    let system = assembled(vec![
        fixed("plugin-probe", PromptOrder::Plugins, PLUGINS_SECTION),
        extension().expect("prompt extension builds"),
    ])
    .await;
    let head = composed(&[]);
    let docs = format!("{DEFAULT_DOCS_LINE}\n");
    let expected = [head.as_str(), PLUGINS_SECTION, docs.as_str()].join(SECTION_SEPARATOR);
    assert_eq!(system, expected);
}

#[tokio::test]
async fn prompt_sections_under_single_owner_render_in_deterministic_order() {
    let reverse_owner = ExtensionBuilder::new("single-owner", "0.1.0", ServiceSet::EMPTY)
        .expect("fixture builder")
        .prompt_section(PromptSection::static_text(
            PromptOrder::DocsLine,
            "SECTION-DOCS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Fallback,
            "SECTION-FALLBACK\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Environment,
            "SECTION-ENVIRONMENT\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            "SECTION-PLUGINS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Rulebook,
            "SECTION-RULEBOOK\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Rules,
            "SECTION-RULES\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Skills,
            "SECTION-SKILLS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Tools,
            "SECTION-TOOLS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::D2,
            "SECTION-D2\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Identity,
            "SECTION-IDENTITY\n".into(),
        ))
        .build()
        .expect("reverse fixture extension");

    let forward_owner = ExtensionBuilder::new("single-owner", "0.1.0", ServiceSet::EMPTY)
        .expect("fixture builder")
        .prompt_section(PromptSection::static_text(
            PromptOrder::Identity,
            "SECTION-IDENTITY\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::D2,
            "SECTION-D2\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Tools,
            "SECTION-TOOLS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Skills,
            "SECTION-SKILLS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Rules,
            "SECTION-RULES\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Rulebook,
            "SECTION-RULEBOOK\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Plugins,
            "SECTION-PLUGINS\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Environment,
            "SECTION-ENVIRONMENT\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::Fallback,
            "SECTION-FALLBACK\n".into(),
        ))
        .prompt_section(PromptSection::static_text(
            PromptOrder::DocsLine,
            "SECTION-DOCS\n".into(),
        ))
        .build()
        .expect("forward fixture extension");

    let reverse_system = assembled(vec![reverse_owner]).await;
    let forward_system = assembled(vec![forward_owner]).await;

    let markers = [
        "SECTION-IDENTITY",
        "SECTION-D2",
        "SECTION-TOOLS",
        "SECTION-SKILLS",
        "SECTION-RULES",
        "SECTION-RULEBOOK",
        "SECTION-PLUGINS",
        "SECTION-ENVIRONMENT",
        "SECTION-FALLBACK",
        "SECTION-DOCS",
    ];

    let mut previous = 0;
    for marker in markers {
        let pos = reverse_system[previous..]
            .find(marker)
            .unwrap_or_else(|| panic!("{marker} missing from {reverse_system:?}"))
            + previous;
        assert!(
            pos >= previous,
            "{marker} out of order in {reverse_system:?}"
        );
        previous = pos + marker.len();
    }

    assert_eq!(
        reverse_system, forward_system,
        "single-owner sections registered in reverse order render byte-identical to forward order"
    );
}

#[tokio::test]
async fn prompt_extension_sections_render_deterministically_when_registered_in_reverse() {
    let reverse_prompt = ExtensionBuilder::new("prompt", "0.1.0", ServiceSet::EMPTY)
        .expect("builder")
        .prompt_section(PromptSection::session(
            PromptOrder::DocsLine,
            Arc::new(DocsSection),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::Instructions,
            Arc::new(InstructionsSection),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::Tools,
            Arc::new(ToolsSection),
        ))
        .prompt_section(PromptSection::session(
            PromptOrder::Identity,
            Arc::new(HeadSection {
                product_name: "dal".into(),
                product_version: env!("CARGO_PKG_VERSION").into(),
            }),
        ))
        .build()
        .expect("reverse prompt extension");

    let forward_prompt = extension().expect("canonical prompt extension");

    let reverse_system = assembled(vec![tool_probe(), reverse_prompt]).await;
    let forward_system = assembled(vec![tool_probe(), forward_prompt]).await;

    assert_eq!(
        reverse_system, forward_system,
        "reverse registration of prompt session sections produces byte-identical assembled prompt"
    );

    let head = composed(&[]);
    let tool_line: Box<str> = Box::from("probe_tool: Probe tool.");
    let with_tools = composed(&[tool_line]);
    let tools = with_tools
        .strip_prefix(head.as_str())
        .expect("tools follows head");
    let docs = format!("{DEFAULT_DOCS_LINE}\n");
    let expected = [head.as_str(), tools, docs.as_str()].join(SECTION_SEPARATOR);

    assert_eq!(reverse_system, expected);
}
