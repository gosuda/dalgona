#![expect(
    clippy::expect_used,
    reason = "regression fixture failures must fail at their specific setup boundary"
)]
//! Regression: letter images must not reach a provider request when the
//! selected model lacks image input capability.
//!
//! The first-input hook already withholds fresh letters from such a model,
//! but replayed history can carry letter images admitted for an earlier
//! image-capable turn. The request boundary negotiates those to words: the
//! text-only model receives text alone while the descriptions stay readable
//! as prompt text and at `letter://`.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, ExtensionBuilder, ModelCx, ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, Expect, ModelId, ModelRequest,
    ModelRoute, Part, Save, ServiceSet, ThinkingLevel, UpdateKind, Workspace,
};
use dal_ext::skills::{BodyInput, SkillRegistration, SkillRegistry, validate_registration};
use dal_provider::{ProviderError, StopReason, StreamEvent};

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(20);
const GRACE: Duration = Duration::from_secs(1);
const TEXT_MODEL: &str = "fixture/text";
const VISION_MODEL: &str = "fixture/vision";
const PNG: &str = "image:image/png";

type Skill = (&'static str, &'static str, bool);
const ALPHA: Skill = ("alpha", "Review the diff carefully.", true);
const BETA: Skill = ("beta", "Write a changelog entry.", true);

struct Seen {
    model: String,
    users: Vec<Vec<String>>,
}

struct Capture {
    seen: Arc<Mutex<Vec<Seen>>>,
}

fn shape(part: &Part) -> String {
    match part {
        Part::Text { text } => format!("text:{text}"),
        Part::Image { mime, .. } | Part::Blob { mime, .. } => format!("image:{mime}"),
    }
}

impl ModelHandler for Capture {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        let users = request
            .context
            .iter()
            .filter_map(|item| match item {
                ContextItem::User { parts } => Some(parts.iter().map(shape).collect()),
                _ => None,
            })
            .collect();
        let model = match &request.model {
            ModelRoute::Synthetic { id } => id.to_string(),
            other => format!("{other:?}"),
        };
        if let Ok(mut seen) = self.seen.lock() {
            seen.push(Seen { model, users });
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

struct Rig {
    root: PathBuf,
    data: PathBuf,
    workspace: Workspace,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Rig {
    fn new(tag: &str) -> Outcome<Self> {
        let root = std::env::temp_dir().join(format!(
            "dal-letter-cap-probe-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("clock")
                .as_nanos()
        ));
        let data = root.join("data");
        let workspace = root.join("workspace");
        std::fs::create_dir_all(&data)?;
        std::fs::create_dir_all(&workspace)?;
        Ok(Self {
            workspace: Workspace::new(workspace)?,
            data,
            root,
            seen: Arc::new(Mutex::new(Vec::new())),
        })
    }

    async fn start(&self, skills: &[Skill], default: &str) -> Outcome<Host> {
        let registry = Arc::new(registry(skills)?);
        let skills_extension = dal_ext::skills_extension(Arc::clone(&registry))?;
        let letter_extension = dal_ext::letter_extension(registry)?;
        let models = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())?
            .model(text_model(&self.seen)?)
            .model(vision_model(&self.seen)?)
            .build()?;
        let default = format!("model = \"{default}\"\n");
        let config = Config::load(
            ConfigProduct::Dalgon,
            &self.data,
            "",
            Some(default.as_str()),
        )?;
        let product = Product {
            name: "dal",
            data_root: self.data.clone(),
            defaults: "",
            extensions: vec![skills_extension, letter_extension, models],
            bundled: Vec::new(),
        };
        let env = Env {
            vars: std::collections::BTreeMap::new(),
            cwd: self.root.join("workspace"),
            sandbox_helper: None,
        };
        Ok(Host::start(product, config, env).await?)
    }

    async fn open(&self, host: &Host, client: &str) -> Outcome<Agent> {
        let session = SessionRef::New {
            workspace: self.workspace.clone(),
            name: None,
        };
        Ok(host.open(session, ClientId::new(client)).await?)
    }

    fn requests(&self) -> Outcome<Vec<(String, Vec<Vec<String>>)>> {
        let seen = self
            .seen
            .lock()
            .map_err(|_| "the request log is poisoned")?;
        Ok(seen
            .iter()
            .map(|seen| (seen.model.clone(), seen.users.clone()))
            .collect())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn model_record(seen: &Arc<Mutex<Vec<Seen>>>, id: &str, image_input: bool) -> Outcome<ModelRecord> {
    Ok(ModelRecord {
        id: ModelId::parse(id)?,
        caps: Caps {
            context_window: Some(100_000),
            thinking: Box::new([ThinkingLevel::Off]),
            tool_use: false,
            image_input,
            custom_grammar: false,
        },
        handler: Arc::new(Capture {
            seen: Arc::clone(seen),
        }),
        export: None,
    })
}

fn text_model(seen: &Arc<Mutex<Vec<Seen>>>) -> Outcome<ModelRecord> {
    model_record(seen, TEXT_MODEL, false)
}

fn vision_model(seen: &Arc<Mutex<Vec<Seen>>>) -> Outcome<ModelRecord> {
    model_record(seen, VISION_MODEL, true)
}

fn registry(skills: &[Skill]) -> Outcome<SkillRegistry> {
    let mut owned = Vec::new();
    for (name, description, letter2image) in skills {
        owned.push(validate_registration(
            "p",
            Path::new("/plugins/p"),
            SkillRegistration {
                name,
                description,
                path: Path::new("SKILL.md"),
                letter2image: *letter2image,
            },
            BodyInput::Bytes(b"# body\n"),
        )?);
    }
    SkillRegistry::merge(&[("p", owned)]).map_err(|conflict| conflict.to_string().into())
}

async fn prompt(agent: &Agent, subscription: &mut Subscription, text: &str) -> Outcome<()> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-turn".into());
        };
        match &update.kind {
            UpdateKind::TurnEnded { .. } => return Ok(()),
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
            _ => {}
        }
    }
}

fn image_count(users: &[Vec<String>]) -> usize {
    users.iter().flatten().filter(|part| *part == PNG).count()
}

#[tokio::test]
async fn switched_to_text_model_receives_no_letter_images() -> Outcome<()> {
    let rig = Rig::new("switch")?;
    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let agent = rig.open(&host, "switch").await?;
    let mut subscription = agent.subscribe(None)?;

    prompt(&agent, &mut subscription, "hello").await?;
    let requests = rig.requests()?;
    assert_eq!(requests.len(), 1, "one request so far: {requests:?}");
    assert_eq!(
        image_count(&requests[0].1),
        2,
        "premise: vision model receives both letters: {requests:?}"
    );

    agent
        .submit(Command::SetModel {
            model: ModelRoute::Synthetic {
                id: TEXT_MODEL.into(),
            },
            save: Save::SessionOnly,
        })
        .await?;
    prompt(&agent, &mut subscription, "again").await?;

    let requests = rig.requests()?;
    assert_eq!(requests.len(), 2, "two requests total: {requests:?}");
    assert_eq!(
        requests[1].0, TEXT_MODEL,
        "the text model answers: {requests:?}"
    );
    assert_eq!(
        image_count(&requests[1].1),
        0,
        "text-only model must receive no letter images: {requests:?}"
    );

    let _report = host.shutdown(GRACE).await;
    Ok(())
}
