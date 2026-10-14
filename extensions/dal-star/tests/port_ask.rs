//! Real-host boundary tests for the ask example plugin.

use std::collections::BTreeMap;
use std::error::Error;
use std::ffi::OsString;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Answer, ClientId, Command, Config, ConfigProduct, Expect, Part, Question, RawJson, Reply,
    ToolOutcomeView, UpdateKind, Workspace,
};
use dal_star::{LoadRoots, PluginSystem, PluginsConfig, load};

const PLUGIN_SOURCE: &str = include_str!("../../../examples/plugins/ask/plugin.star");
const TOOL_NAME: &str = "ask__questionnaire";
const QUESTION_IDS: [&str; 8] = [
    "first", "second", "third", "fourth", "fifth", "sixth", "seventh", "eighth",
];
const ASK_ARGS: &str = r#"{"questions":[{"id":"first","prompt":"First?","options":["a","b"],"allow_other":false},{"id":"second","prompt":"Second?"},{"id":"third","prompt":"Third?","options":["a","b","c","d","e","f","g","h","i","j"]},{"id":"fourth","prompt":"Fourth?"},{"id":"fifth","prompt":"Fifth?","options":["left","right","middle"]},{"id":"sixth","prompt":"Sixth?"},{"id":"seventh","prompt":"Seventh?","options":["yes","no"]},{"id":"eighth","prompt":"Eighth?"}]}"#;

fn ask_replay(args: &str) -> String {
    let template = r#"{"kind":"events","events":[{"type":"tool_call_started","id":"ask-call","name":"__TOOL__"},{"type":"tool_calls_done","calls":[{"id":"ask-call","name":"__TOOL__","args":{"kind":"parsed","value":__ARGS__}}]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"tool_use"}]}
{"kind":"events","events":[{"type":"text_delta","text":"finished"},{"type":"tool_calls_done","calls":[]},{"type":"usage","usage":{"input_tokens":1,"cached_input_tokens":0,"output_tokens":1,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}},{"type":"stop","reason":"end_turn"}]}
"#;
    template
        .replace("__TOOL__", TOOL_NAME)
        .replace("__ARGS__", args)
}

async fn start_host(
    args: &str,
) -> Result<(tempfile::TempDir, Host, Agent), Box<dyn Error + Send + Sync>> {
    let root = tempfile::tempdir()?;
    let data = root.path().join("data");
    let plugin_dir = data.join("plugins/ask");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(plugin_dir.join("plugin.star"), PLUGIN_SOURCE)?;

    let plugin_config = PluginsConfig::default();
    let roots = LoadRoots {
        data_root: data.clone(),
        bundled: Vec::new(),
    };
    let generation = load(&roots, &plugin_config)?;
    let extensions = PluginSystem::new(generation, roots, plugin_config).extensions()?;

    let fixture = data.join("ask-scripted.jsonl");
    std::fs::write(&fixture, ask_replay(args))?;
    let user = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6\"\n[providers.scripted]\nfixture = {:?}\n",
        fixture.to_string_lossy()
    );
    let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(&user))?;

    let workspace = root.path().join("workspace");
    std::fs::create_dir_all(&workspace)?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions,
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
            SessionRef::Ephemeral {
                workspace: Workspace::new(workspace)?,
            },
            ClientId::new("ask-test"),
        )
        .await?;
    Ok((root, host, agent))
}

fn answer_for(
    index: usize,
    question: &Question,
) -> Result<(Answer, String), Box<dyn Error + Send + Sync>> {
    match question {
        Question::Select { options, .. } => {
            let label = options
                .last()
                .ok_or_else(|| std::io::Error::other("select request has no options"))?
                .label
                .to_string();
            let json = sonic_rs::to_string(&label)?;
            Ok((Answer::Value(RawJson::parse(&json)?), label))
        }
        Question::Text { .. } => {
            let text = format!("typed-{index}");
            let json = sonic_rs::to_string(&text)?;
            Ok((Answer::Value(RawJson::parse(&json)?), text))
        }
        other => Err(std::io::Error::other(format!(
            "questionnaire raised an unexpected request: {other:?}"
        ))
        .into()),
    }
}

async fn run_connected(
    agent: &Agent,
    mut subscription: Subscription,
) -> Result<(ToolOutcomeView, Vec<Question>, Vec<String>), Box<dyn Error + Send + Sync>> {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Ask the questionnaire and use every answer.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }

    let mut settled = None;
    let mut questions = Vec::new();
    let mut answers = Vec::new();
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            break;
        };
        match &update.kind {
            UpdateKind::RequestOpened(request) => {
                let index = questions.len();
                let (answer, text) = answer_for(index + 1, &request.question)?;
                questions.push(request.question.clone());
                answers.push(text);
                agent.answer(request.id, answer).await?;
            }
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } if settled.is_some() => break,
            _ => {}
        }
    }
    let outcome =
        settled.ok_or_else(|| std::io::Error::other("the real host did not settle ask"))?;
    Ok((outcome, questions, answers))
}

async fn run_without_controller(
    agent: &Agent,
) -> Result<(ToolOutcomeView, usize), Box<dyn Error + Send + Sync>> {
    let mut subscription = agent.subscribe_listen(None)?;
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "Ask the questionnaire without a controller.".into(),
            }],
        })
        .await?;
    if !matches!(reply, Reply::Accepted { .. }) {
        return Err(std::io::Error::other(format!("prompt was not accepted: {reply:?}")).into());
    }

    let mut settled = None;
    let mut opened = 0;
    loop {
        let delivery = tokio::time::timeout(Duration::from_secs(5), subscription.next()).await?;
        let Some(Delivery::Update(update)) = delivery else {
            break;
        };
        match &update.kind {
            UpdateKind::RequestOpened(_) => opened += 1,
            UpdateKind::ToolSettled { outcome, .. } => settled = Some(outcome.clone()),
            UpdateKind::TurnEnded { .. } if settled.is_some() => break,
            _ => {}
        }
    }
    let outcome =
        settled.ok_or_else(|| std::io::Error::other("the real host did not settle ask"))?;
    Ok((outcome, opened))
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_port_uses_real_controller_answers_and_preserves_every_id() {
    let (_root, host, agent) = start_host(ASK_ARGS)
        .await
        .expect("ask host starts from the example source");
    let subscription = agent
        .subscribe(None)
        .expect("answering controller attaches");
    let result = run_connected(&agent, subscription).await;
    let _ = host.shutdown(Duration::from_secs(1)).await;
    let (outcome, questions, answers) = result.expect("ask questionnaire settles");

    assert!(
        !outcome.is_error,
        "ask must return successful plugin data: {outcome:?}"
    );
    assert_eq!(
        questions.len(),
        QUESTION_IDS.len(),
        "all eight questions run"
    );
    assert_eq!(answers.len(), QUESTION_IDS.len());
    let expected = QUESTION_IDS
        .iter()
        .zip(answers)
        .map(|(id, answer)| format!("{id}: {answer}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(outcome.text.as_ref(), expected);

    let selects = questions
        .iter()
        .filter_map(|question| match question {
            Question::Select { options, .. } => Some(options),
            _ => None,
        })
        .collect::<Vec<_>>();
    assert_eq!(selects.len(), 4, "four option questions use select");
    assert_eq!(
        selects[0].len(),
        3,
        "two declared options plus Type an answer"
    );
    assert_eq!(
        selects[1].len(),
        11,
        "ten declared options plus Type an answer"
    );
    assert_eq!(
        selects[2].len(),
        4,
        "three declared options plus Type an answer"
    );
    assert_eq!(
        selects[3].len(),
        3,
        "two declared options plus Type an answer"
    );
    for options in selects {
        assert_eq!(
            options.last().map(|choice| choice.label.as_ref()),
            Some("Type an answer")
        );
    }
    assert_eq!(
        questions
            .iter()
            .filter(|question| matches!(question, Question::Text { .. }))
            .count(),
        4,
        "questions without options use free text"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn ask_port_fails_closed_without_an_answering_controller() {
    let (_root, host, agent) = start_host(ASK_ARGS)
        .await
        .expect("ask host starts from the example source");
    let result = run_without_controller(&agent).await;
    let _ = host.shutdown(Duration::from_secs(1)).await;
    let (outcome, opened) = result.expect("headless ask questionnaire settles");

    assert_eq!(
        opened, 0,
        "no controller means no blocking request is opened"
    );
    assert!(
        !outcome.is_error,
        "headless ask is a normal plugin result: {outcome:?}"
    );
    let expected = QUESTION_IDS
        .iter()
        .map(|id| format!("{id}: no answer"))
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(outcome.text.as_ref(), expected);
}
