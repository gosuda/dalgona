//! Skill invocation through a real host: a prompt that names skills hands
//! their bodies to the model, `/skill:<name>` and an exact `/skill <name>`
//! submit that same prompt, and `/skill <prefix>` lists candidates and
//! starts nothing.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Expect, ModelId, ModelRequest,
    Output, Part, Reply, ServiceSet, Stop, ThinkingLevel, UpdateKind, Workspace,
};
use dal_ext::skills::{BodyInput, SkillRegistration, SkillRegistry, validate_registration};
use dal_provider::{ProviderError, StopReason, StreamEvent};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(20);

/// Records the user-visible text of every request, then ends the turn.
struct Recorder {
    requests: Arc<Mutex<Vec<String>>>,
}

impl ModelHandler for Recorder {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let mut text = String::new();
        for item in request.context.iter() {
            if let ContextItem::User { parts } = item {
                for part in parts {
                    if let Part::Text { text: piece } = part {
                        text.push_str(piece);
                        text.push('\n');
                    }
                }
            }
        }
        if let Ok(mut requests) = self.requests.lock() {
            requests.push(text);
        }
        Box::pin(async {
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
            Ok(EventStream::new(futures::stream::iter(events), || {}))
        })
    }
}

fn registry() -> Outcome<SkillRegistry> {
    let skill = |plugin: &str, name: &str, description: &str, body: &str| {
        validate_registration(
            plugin,
            &Path::new("/plugins").join(plugin),
            SkillRegistration {
                name,
                description,
                path: Path::new("SKILL.md"),
                letter2image: false,
            },
            BodyInput::Bytes(body.as_bytes()),
        )
    };
    let owned_by_p = vec![
        skill(
            "p",
            "find-anything",
            "Pick a search surface.",
            "# FIND-BODY\nuse find\n",
        )?,
        skill(
            "p",
            "find-files",
            "List files by name.",
            "# FILES-BODY\nuse find\n",
        )?,
    ];
    let owned_by_q = vec![
        skill(
            "q",
            "review",
            "Review a diff.",
            "# REVIEW-BODY\nread the diff\n",
        )?,
        skill(
            "q",
            "delegate",
            "Hand work off.",
            "# DELEGATE-BODY\nwrite a contract\n",
        )?,
    ];
    SkillRegistry::merge(&[("p", owned_by_p), ("q", owned_by_q)])
        .map_err(|conflict| conflict.to_string().into())
}

async fn host(root: &Path, requests: &Arc<Mutex<Vec<String>>>) -> Outcome<Host> {
    let skills = dal_ext::skills_extension(Arc::new(registry()?))?;
    let model = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())?
        .model(ModelRecord {
            id: ModelId::parse("fixture/end")?,
            caps: Caps {
                context_window: Some(100_000),
                thinking: Box::new([ThinkingLevel::Off]),
                tool_use: false,
                image_input: false,
                custom_grammar: false,
            },
            handler: Arc::new(Recorder {
                requests: Arc::clone(requests),
            }),
            export: None,
        })
        .build()?;
    let data = root.join("data");
    std::fs::create_dir_all(&data)?;
    let config = Config::load(
        ConfigProduct::Dalgon,
        &data,
        "",
        Some("model = \"fixture/end\"\n"),
    )?;
    let product = Product {
        name: "dal",
        data_root: data,
        defaults: "",
        extensions: vec![skills, model],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::new(),
        cwd: root.join("workspace"),
        sandbox_helper: None,
    };
    Ok(Host::start(product, config, env).await?)
}

async fn run(agent: &Agent, name: &str, args: &str) -> Outcome<Reply> {
    Ok(agent
        .submit(Command::Run {
            name: name.into(),
            args: args.into(),
            expected: None,
        })
        .await?)
}

async fn turn_ends(subscription: &mut Subscription) -> Outcome<()> {
    while let Some(delivery) = tokio::time::timeout(WAIT, subscription.next()).await? {
        if let Delivery::Update(update) = delivery
            && let UpdateKind::TurnEnded { stop, .. } = &update.kind
        {
            assert_eq!(*stop, Stop::EndTurn, "the fixture turn ends cleanly");
            return Ok(());
        }
    }
    Err("subscription closed before the turn ended".into())
}

fn requests_so_far(requests: &Arc<Mutex<Vec<String>>>) -> Outcome<Vec<String>> {
    Ok(requests
        .lock()
        .map_err(|_| "the recorder lock is poisoned")?
        .clone())
}

#[tokio::test]
async fn skill_prefix_lists_and_runs_nothing_while_named_skills_reach_the_model() -> Outcome<()> {
    let root = std::env::temp_dir().join(format!("dal-ext-skill-invoke-{}", std::process::id()));
    std::fs::create_dir_all(root.join("workspace"))?;
    let requests = Arc::new(Mutex::new(Vec::new()));
    let host = host(&root, &requests).await?;
    let agent = host
        .open(
            SessionRef::New {
                workspace: Workspace::new(root.join("workspace"))?,
                name: None,
            },
            ClientId::new("skill-invoke"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;

    // `/skill find-` matches two names, prints both, and nothing
    // resolves and nothing runs.
    let reply = run(&agent, "skill", "find-").await?;
    assert_eq!(
        reply,
        Reply::Done(Output::Text(
            "Skills matching \"find-\" (2):\n\
             - find-anything: Pick a search surface.\n\
             - find-files: List files by name.\n\
             Nothing was run. Run one with /skill:<name> or $<name>."
                .into()
        ))
    );
    assert_eq!(requests_so_far(&requests)?, Vec::<String>::new());

    // A `$` token in a plain prompt: the body of that skill and no other.
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "check with $find-anything now".into(),
            }],
        })
        .await?;
    turn_ends(&mut subscription).await?;
    let seen = requests_so_far(&requests)?;
    let [first] = seen.as_slice() else {
        return Err(format!("one request so far: {seen:?}").into());
    };
    assert_eq!(
        first
            .matches("<skill name=\"find-anything\">\n# FIND-BODY\nuse find\n</skill>")
            .count(),
        1,
        "{first}"
    );
    assert!(first.contains("check with $find-anything now"), "{first}");
    assert!(
        !first.contains("FILES-BODY") && !first.contains("REVIEW-BODY"),
        "{first}"
    );

    // `/skill:<name>` with a `$plugin:name` token: both bodies, mention order.
    let reply = run(&agent, "skill:review", "then $p:find-files").await?;
    assert_eq!(reply, Reply::Done(Output::Nothing));
    turn_ends(&mut subscription).await?;
    let seen = requests_so_far(&requests)?;
    let Some(second) = seen.get(1) else {
        return Err(format!("a second request: {seen:?}").into());
    };
    let review = second.find("<skill name=\"review\">\n# REVIEW-BODY\nread the diff\n</skill>");
    let files = second.find("<skill name=\"find-files\">\n# FILES-BODY\nuse find\n</skill>");
    assert!(
        matches!((review, files), (Some(r), Some(f)) if r < f),
        "{second}"
    );
    assert!(
        second.contains("/skill:review then $p:find-files"),
        "{second}"
    );

    // An exact name after `/skill` runs that skill, the same prompt.
    let reply = run(&agent, "skill", "delegate").await?;
    assert_eq!(reply, Reply::Done(Output::Nothing));
    turn_ends(&mut subscription).await?;
    let seen = requests_so_far(&requests)?;
    let Some(third) = seen.get(2) else {
        return Err(format!("a third request: {seen:?}").into());
    };
    assert_eq!(
        third
            .matches("<skill name=\"delegate\">\n# DELEGATE-BODY\nwrite a contract\n</skill>")
            .count(),
        1,
        "{third}"
    );
    assert!(third.contains("/skill:delegate"), "{third}");

    // A non-exact prefix after all that still starts no turn.
    let reply = run(&agent, "skill", "deleg").await?;
    assert!(
        matches!(&reply, Reply::Done(Output::Text(text)) if text.contains("- delegate: ")),
        "{reply:?}"
    );
    assert_eq!(requests_so_far(&requests)?.len(), 3);

    let _ = std::fs::remove_dir_all(&root);
    Ok(())
}
