#![expect(
    clippy::expect_used,
    reason = "integration fixture failures must fail at their specific setup boundary"
)]
//! The `letter` first-input hook through a real host: skill descriptions
//! marked `letter2image` reach the model as PNG parts ahead of the first
//! prompt only when the turn's model reads images.
//!
//! The host is real (`Host::start`, real session actor, real journal on a
//! scratch directory). The only fake is the model: a deterministic in-process
//! handler that records the user parts of every request it receives, so the
//! assertions read what the provider would have been sent. Journal records are
//! read back from the scratch data root and decoded strictly.
//!
//! Covered here: an image-capable model gets the letters once and a resume
//! adds nothing; a model with `image_input = false` (the reported scenario)
//! receives ordinary text and no letter record is persisted; the session's
//! selected model, not the configured default, decides; a stored set that no
//! longer matches the loaded skills stays frozen and never grows duplicate
//! ids; a stored set draws its fallback diagnostics only once; a failed
//! letter hook publishes its cause; a well-formed row with an unsupported
//! version fails the hook without a duplicate id; a record prefix left by a
//! crash is completed without a duplicate or conflicting id; and a skill
//! shows by name only while its image is in the request.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx,
    ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryKind, Expect, InputEvent,
    InputVerdict, JournalPart, ModelId, ModelRequest, ModelRoute, PageReq, Part, RawJson, Record,
    Save, ServiceSet, SessionId, Stop, ThinkingLevel, Timestamp, TurnId, UpdateKind, Workspace,
};
use dal_ext::skills::{BodyInput, SkillRegistration, SkillRegistry, validate_registration};
use dal_ext::{SkillLetterRecord, letter};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use tokio::sync::Notify;

type Outcome<T> = Result<T, Box<dyn std::error::Error + Send + Sync>>;

const WAIT: Duration = Duration::from_secs(20);
const GRACE: Duration = Duration::from_secs(1);

const TEXT_MODEL: &str = "fixture/text";
const VISION_MODEL: &str = "fixture/vision";

/// The shape of one provider-facing letter image part.
const PNG: &str = "image:image/png";

/// One skill as the registry builder takes it: name, description, marked.
type Skill = (&'static str, &'static str, bool);

const ALPHA: Skill = ("alpha", "Review the diff carefully.", true);
const BETA: Skill = ("beta", "Write a changelog entry.", true);
const GAMMA: Skill = ("gamma", "Summarize the thread.", false);

/// What the model saw in one request: its id, the system prompt, and the
/// user parts per message.
struct Seen {
    model: String,
    system: String,
    users: Vec<Vec<String>>,
}

struct Capture {
    seen: Arc<Mutex<Vec<Seen>>>,
}

/// Renders one part as `text:<text>` or `image:<mime>`; a spilled image
/// blob counts as the image it stands for.
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
            seen.push(Seen {
                model,
                system: request.system.to_string(),
                users,
            });
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

/// A scratch data root, workspace, and the model's request log; the data
/// root outlives host restarts so a session can be resumed.
struct Rig {
    root: PathBuf,
    data: PathBuf,
    workspace: Workspace,
    seen: Arc<Mutex<Vec<Seen>>>,
}

impl Rig {
    fn new(tag: &str) -> Outcome<Self> {
        let root = std::env::temp_dir().join(format!(
            "dal-letter-first-prompt-{tag}-{}-{}",
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

    /// Starts a real host with the skills and letter extensions over
    /// `skills`, a text-only and an image-capable synthetic model, and
    /// `default` as the configured model.
    async fn start(&self, skills: &[Skill], default: &str) -> Outcome<Host> {
        self.start_with(skills, default, Vec::new()).await
    }

    /// [`Rig::start`] plus `extra` extensions registered after the letter
    /// extension, so their input hooks fold after the letter hook's.
    async fn start_with(
        &self,
        skills: &[Skill],
        default: &str,
        extra: Vec<Extension>,
    ) -> Outcome<Host> {
        let registry = Arc::new(registry(skills)?);
        let skills_extension = dal_ext::skills_extension(Arc::clone(&registry))?;
        let letter_extension = dal_ext::letter_extension(registry)?;
        let model = |id: &str, image_input: bool| -> Outcome<ModelRecord> {
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
                    seen: Arc::clone(&self.seen),
                }),
                export: None,
            })
        };
        let models = ExtensionBuilder::new("fixture", "0.1.0", ServiceSet::default())?
            .model(model(TEXT_MODEL, false)?)
            .model(model(VISION_MODEL, true)?)
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
            extensions: [vec![skills_extension, letter_extension, models], extra].concat(),
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

    async fn resume(&self, host: &Host, id: SessionId, client: &str) -> Outcome<Agent> {
        let session = SessionRef::Resume {
            key: id.to_string().into(),
            workspace: self.workspace.clone(),
        };
        Ok(host.open(session, ClientId::new(client)).await?)
    }

    /// The user parts of every request so far, oldest request first.
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

    /// The system prompt of every request so far, oldest request first.
    fn systems(&self) -> Outcome<Vec<String>> {
        let seen = self
            .seen
            .lock()
            .map_err(|_| "the request log is poisoned")?;
        Ok(seen.iter().map(|seen| seen.system.clone()).collect())
    }

    /// Every decoded letter record of the journal, in journal order.
    fn letter_records(&self) -> Outcome<Vec<SkillLetterRecord>> {
        let mut letters = Vec::new();
        for record in journal_records(&self.data)? {
            let Record::Ext {
                ext, kind, body, ..
            } = record
            else {
                continue;
            };
            if ext.as_ref() == "letter" && kind.as_ref() == "letter" {
                letters.push(body.decode_as::<SkillLetterRecord>()?);
            }
        }
        Ok(letters)
    }

    /// The journal file of `session`.
    fn journal_of(&self, session: SessionId) -> Outcome<PathBuf> {
        let mut pending = vec![self.data.clone()];
        let wanted = session.to_string();
        while let Some(directory) = pending.pop() {
            for found in std::fs::read_dir(&directory)? {
                let path = found?.path();
                if path.is_dir() {
                    pending.push(path);
                } else if path.file_name().is_some_and(|name| name == "journal.jsonl")
                    && path
                        .parent()
                        .and_then(Path::file_name)
                        .is_some_and(|name| *name == *wanted)
                {
                    return Ok(path);
                }
            }
        }
        Err("the session journal is missing".into())
    }
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
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

/// Every durable journal record across the scratch data root.
fn journal_records(data: &Path) -> Outcome<Vec<Record>> {
    let mut records = Vec::new();
    let mut pending = vec![data.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for found in std::fs::read_dir(&directory)? {
            let path = found?.path();
            if path.is_dir() {
                pending.push(path);
            } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
                let text = std::fs::read_to_string(&path)?;
                for line in text.lines().filter(|line| !line.trim().is_empty()) {
                    records.push(dal_core::decode(line.as_bytes())?.record);
                }
            }
        }
    }
    Ok(records)
}

/// A notice as the session published it: kind and text.
type Note = (String, String);

/// Submits one prompt and drains the subscription until the turn ends
/// cleanly, returning every notice published meanwhile.
async fn prompt(agent: &Agent, subscription: &mut Subscription, text: &str) -> Outcome<Vec<Note>> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    let mut notes = Vec::new();
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "turn timed out")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("subscription closed mid-turn".into());
        };
        match &update.kind {
            UpdateKind::TurnEnded { stop, .. } => {
                assert_eq!(*stop, Stop::EndTurn, "the turn ends cleanly");
                return Ok(notes);
            }
            UpdateKind::Notice(notice)
                if matches!(notice.kind.as_ref(), "driver.failed" | "turn.failed") =>
            {
                return Err(format!("turn failed: {}", notice.text).into());
            }
            UpdateKind::Notice(notice) => {
                notes.push((notice.kind.to_string(), notice.text.to_string()));
            }
            _ => {}
        }
    }
}

/// The texts of the `letter` notices among `notes`.
fn letter_notices(notes: &[Note]) -> Vec<&str> {
    notes
        .iter()
        .filter(|(kind, _)| kind == "letter")
        .map(|(_, text)| text.as_str())
        .collect()
}

/// Builds the expected user parts of one message: `n` letter images, then
/// the prompt text.
fn with_letters(n: usize, text: &str) -> Vec<String> {
    let mut parts = vec![PNG.to_owned(); n];
    parts.push(format!("text:{text}"));
    parts
}

fn plain(text: &str) -> Vec<String> {
    with_letters(0, text)
}

fn ids(records: &[SkillLetterRecord]) -> Vec<Option<u8>> {
    records.iter().map(SkillLetterRecord::validated).collect()
}

#[tokio::test]
async fn image_model_gets_the_letters_once_and_resume_adds_none() -> Outcome<()> {
    let rig = Rig::new("image-model")?;
    let host = rig.start(&[ALPHA, BETA, GAMMA], VISION_MODEL).await?;
    let agent = rig.open(&host, "image-model").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;

    let notes = prompt(&agent, &mut subscription, "hello").await?;
    assert!(
        letter_notices(&notes).is_empty(),
        "an image model needs no notice"
    );
    let requests = rig.requests()?;
    assert_eq!(requests[0].0, VISION_MODEL);
    assert_eq!(
        requests[0].1,
        vec![with_letters(2, "hello")],
        "the first request's first user item is the two marked letters, then the text"
    );
    let systems = rig.systems()?;
    assert!(
        systems[0].contains("- alpha\n- beta"),
        "an image model gets name-only lines for lettered skills"
    );
    for leaked in ["Review the diff carefully.", "Write a changelog entry."] {
        assert!(
            !systems[0].contains(leaked),
            "no lettered description leaks into the prompt as text: {leaked}"
        );
    }
    assert!(
        systems[0].contains("- gamma: Summarize the thread."),
        "the unmarked skill still renders its description"
    );
    let letters = rig.letter_records()?;
    assert_eq!(ids(&letters), vec![Some(1), Some(2)]);
    assert_eq!(
        letters
            .iter()
            .map(|record| &*record.skill)
            .collect::<Vec<_>>(),
        ["alpha", "beta"],
        "bytewise name order; the unmarked skill draws nothing"
    );

    prompt(&agent, &mut subscription, "again").await?;
    assert_eq!(
        rig.requests()?[1].1,
        vec![with_letters(2, "hello"), plain("again")],
        "the second turn adds no image"
    );
    assert_eq!(
        rig.letter_records()?.len(),
        2,
        "the second turn stores nothing"
    );

    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let resumed = rig.resume(&host, session, "image-model-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    prompt(&resumed, &mut subscription, "third").await?;
    assert_eq!(
        rig.requests()?[2].1,
        vec![with_letters(2, "hello"), plain("again"), plain("third")],
        "the resumed session replays the same two images and adds none"
    );
    assert_eq!(rig.letter_records()?.len(), 2, "a resume stores nothing");

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn text_only_model_gets_ordinary_text_and_no_letter_records() -> Outcome<()> {
    let rig = Rig::new("text-model")?;
    let host = rig.start(&[ALPHA, BETA], TEXT_MODEL).await?;
    let agent = rig.open(&host, "text-model").await?;
    let mut subscription = agent.subscribe(None)?;

    let notes = prompt(&agent, &mut subscription, "hello").await?;
    let requests = rig.requests()?;
    assert_eq!(requests[0].0, TEXT_MODEL);
    assert_eq!(
        requests[0].1,
        vec![plain("hello")],
        "an image_input=false model receives the ordinary text prompt"
    );
    assert!(
        rig.systems()?[0].contains("- alpha: Review the diff carefully."),
        "a text-only model keeps the description in the prompt: no letter hides it"
    );
    assert_eq!(
        letter_notices(&notes),
        [letter::no_image_model_notice()],
        "one notice says why"
    );
    assert!(rig.letter_records()?.is_empty(), "no durable letter record");

    let notes = prompt(&agent, &mut subscription, "again").await?;
    assert_eq!(rig.requests()?[1].1, vec![plain("hello"), plain("again")]);
    assert!(
        letter_notices(&notes).is_empty(),
        "the notice appears once per session, not on every input"
    );
    assert!(rig.letter_records()?.is_empty(), "still no letter record");

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn selected_image_model_attaches_letters_over_a_text_default() -> Outcome<()> {
    let rig = Rig::new("select-image")?;
    let host = rig.start(&[ALPHA, BETA], TEXT_MODEL).await?;
    let agent = rig.open(&host, "select-image").await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::SetModel {
            model: ModelRoute::Synthetic {
                id: VISION_MODEL.into(),
            },
            save: Save::SessionOnly,
        })
        .await?;

    let notes = prompt(&agent, &mut subscription, "hello").await?;
    let requests = rig.requests()?;
    assert_eq!(requests[0].0, VISION_MODEL, "the selected model answers");
    assert_eq!(
        requests[0].1,
        vec![with_letters(2, "hello")],
        "the selected image model, not the text default, decides"
    );
    assert_eq!(letter_notices(&notes), [] as [&str; 0]);
    assert_eq!(ids(&rig.letter_records()?), vec![Some(1), Some(2)]);

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn selected_text_model_withholds_letters_over_an_image_default() -> Outcome<()> {
    let rig = Rig::new("select-text")?;
    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let agent = rig.open(&host, "select-text").await?;
    let mut subscription = agent.subscribe(None)?;
    agent
        .submit(Command::SetModel {
            model: ModelRoute::Synthetic {
                id: TEXT_MODEL.into(),
            },
            save: Save::SessionOnly,
        })
        .await?;

    let notes = prompt(&agent, &mut subscription, "hello").await?;
    let requests = rig.requests()?;
    assert_eq!(requests[0].0, TEXT_MODEL, "the selected model answers");
    assert_eq!(
        requests[0].1,
        vec![plain("hello")],
        "the selected text model, not the image default, decides"
    );
    assert_eq!(letter_notices(&notes), [letter::no_image_model_notice()]);
    assert!(rig.letter_records()?.is_empty());

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn changed_skill_set_freezes_the_stored_letters_without_duplicates() -> Outcome<()> {
    let rig = Rig::new("mismatch")?;
    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let agent = rig.open(&host, "mismatch").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "hello").await?;
    assert_eq!(ids(&rig.letter_records()?), vec![Some(1)]);
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    // A later start loads a different description for `alpha` and a second
    // marked skill: the stored set no longer positionally matches.
    let changed: Skill = ("alpha", "Review the diff with great care.", true);
    let host = rig.start(&[changed, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "mismatch-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;

    assert_eq!(
        rig.requests()?[1].1,
        vec![with_letters(1, "hello"), plain("again")],
        "the frozen set adds no image to the later prompt"
    );
    let letters = rig.letter_records()?;
    assert_eq!(
        ids(&letters),
        vec![Some(1)],
        "the mismatched stored set stays one record: no duplicate ids"
    );
    assert_eq!(&*letters[0].skill, "alpha");
    assert!(
        letter_notices(&notes).is_empty(),
        "a frozen set is not an error"
    );

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// Collects the message of every tracing event on this thread.
#[derive(Clone, Default)]
struct Events(Arc<Mutex<Vec<String>>>);

struct Message(String);

impl tracing::field::Visit for Message {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        if field.name() == "message" {
            self.0 = format!("{value:?}");
        }
    }
}

impl tracing::Subscriber for Events {
    fn enabled(&self, _metadata: &tracing::Metadata<'_>) -> bool {
        true
    }

    fn new_span(&self, _attributes: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _span: &tracing::span::Id, _values: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _span: &tracing::span::Id, _follows: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        let mut message = Message(String::new());
        event.record(&mut message);
        if let Ok(mut events) = self.0.lock() {
            events.push(message.0);
        }
    }

    fn enter(&self, _span: &tracing::span::Id) {}

    fn exit(&self, _span: &tracing::span::Id) {}
}

#[tokio::test]
async fn stored_letters_log_each_fallback_once_not_on_every_input() -> Outcome<()> {
    // `beta` marks letter2image but its description holds an undrawable
    // codepoint, so it stays text and the assembly always logs one fallback.
    let undrawable: Skill = ("beta", "smile \u{1F600} here", true);
    let events = Events::default();
    let _scoped = tracing::subscriber::set_default(events.clone());
    let rig = Rig::new("fallback-log")?;
    let host = rig.start(&[ALPHA, undrawable], VISION_MODEL).await?;
    let agent = rig.open(&host, "fallback-log").await?;
    let mut subscription = agent.subscribe(None)?;

    for text in ["one", "two", "three"] {
        prompt(&agent, &mut subscription, text).await?;
    }

    let fallbacks = events
        .0
        .lock()
        .map_err(|_| "the event log is poisoned")?
        .iter()
        .filter(|message| message.contains("fell back to text"))
        .count();
    assert_eq!(
        fallbacks, 1,
        "the fallback is diagnosed when the set is stored, not on every input"
    );
    assert_eq!(
        rig.requests()?[2].1,
        vec![with_letters(1, "one"), plain("two"), plain("three")]
    );
    assert_eq!(ids(&rig.letter_records()?), vec![Some(1)]);

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn failed_letter_hook_publishes_its_cause() -> Outcome<()> {
    let rig = Rig::new("hook-failure")?;
    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let agent = rig.open(&host, "hook-failure").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "hello").await?;
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    // Damage the journal the way a foreign writer could: one `letter` row
    // that the strict record schema rejects.
    let damaged = dal_core::encode(&Record::Ext {
        at: Timestamp::now(),
        ext: "letter".into(),
        kind: "letter".into(),
        body: RawJson::parse(r#"{"v":1,"id":"2","kind":"skill","bogus":true}"#)?,
    })?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(rig.journal_of(session)?)?
        .write_all(&damaged)?;

    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "hook-failure-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;

    let notices = letter_notices(&notes);
    assert_eq!(notices.len(), 1, "one letter notice for the failed hook");
    let cause = notices[0]
        .strip_prefix("letter: skill letters were not persisted: ")
        .expect("the notice names the failure");
    assert!(
        cause.contains("bogus"),
        "the notice carries the real cause, not a fixed text: {cause}"
    );
    assert_eq!(
        rig.requests()?[1].1,
        vec![with_letters(1, "hello"), plain("again")],
        "the failed hook keeps the running input as it was"
    );

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Delivery-contract witnesses (`letter_red_*`).
//
// One invariant, read off what the model actually received: a skill whose
// prompt line shows only its name (`- alpha`) promises that a letter image
// carries the description, so every request must hold at least as many image
// parts as it has name-only lettered lines. Records on disk are never proof
// of delivery.
//
// Prompt acceptance journals a turn reservation first, so the journal is
// file-backed before any input hook runs; every witness below delivers on an
// image model directly. The text-model seed in `letter_red_f2_failed_hook_*`
// exists only so the resume has a normal user entry and the damaged row id
// stays unique.
// ---------------------------------------------------------------------------

/// The lettered skills every witness registers: `ALPHA` and `BETA`.
const LETTERED: [&str; 2] = ["alpha", "beta"];

/// Letter image parts across every user message of one request.
fn image_parts_in(users: &[Vec<String>]) -> usize {
    users.iter().flatten().filter(|part| *part == PNG).count()
}

/// How many lettered skills the system prompt shows by name only.
fn name_only_lines(system: &str) -> usize {
    system
        .lines()
        .filter(|line| {
            line.strip_prefix("- ")
                .is_some_and(|name| LETTERED.contains(&name))
        })
        .count()
}

/// Asserts every captured request carries as many image parts as the prompt
/// has name-only lettered lines.
fn assert_names_are_backed_by_images(rig: &Rig, notes: &[Note]) -> Outcome<()> {
    let systems = rig.systems()?;
    let requests = rig.requests()?;
    for (index, (system, (model, users))) in systems.iter().zip(&requests).enumerate() {
        let hidden = name_only_lines(system);
        let images = image_parts_in(users);
        assert!(
            hidden <= images,
            "request {index} (model {model}) hides {hidden} skill description(s) behind \
             letter images but carries {images}: users={users:?}; \
             letter notices={:?}",
            letter_notices(notes),
        );
    }
    Ok(())
}

/// Submits one prompt without waiting for the turn to end.
async fn submit_prompt(agent: &Agent, text: &str) -> Outcome<TurnId> {
    let reply = agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text { text: text.into() }],
        })
        .await?;
    match reply {
        dal_core::Reply::Accepted { turn, .. } => Ok(turn),
        other => Err(format!("the prompt was not accepted: {other:?}").into()),
    }
}

/// Drains the subscription until `turn` ends and returns its stop reason.
async fn drain_stop(subscription: &mut Subscription, turn: TurnId) -> Outcome<Stop> {
    loop {
        let delivery = tokio::time::timeout(WAIT, subscription.next())
            .await
            .map_err(|_| "the turn did not end in time")?;
        let Some(Delivery::Update(update)) = delivery else {
            return Err("the subscription closed before the turn ended".into());
        };
        if let UpdateKind::TurnEnded { turn: seen, stop } = &update.kind
            && *seen == turn
        {
            return Ok(*stop);
        }
    }
}

/// Waits until some captured request carries `text` as a user part.
async fn wait_request_with(rig: &Rig, text: &str) -> Outcome<()> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let found = rig.requests()?.iter().any(|(_, users)| {
            users
                .iter()
                .flatten()
                .any(|part| part.as_str().strip_prefix("text:") == Some(text))
        });
        if found {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            return Err(format!(
                "no request carried {text:?}; requests={:?}",
                rig.requests()?
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every extension-record (ext, kind) pair in the journal, in order.
/// Delivery markers are records, not entries, so the entry dump cannot see
/// them; this can.
fn ext_rows(rig: &Rig) -> Outcome<Vec<(String, String)>> {
    let mut rows = Vec::new();
    for record in journal_records(&rig.data)? {
        if let Record::Ext { ext, kind, .. } = record {
            rows.push((ext.to_string(), kind.to_string()));
        }
    }
    Ok(rows)
}

/// Whether any journaled user entry carries image parts.
fn journal_user_images(agent: &Agent) -> Outcome<bool> {
    Ok(agent
        .view(PageReq::default())?
        .entries
        .items
        .iter()
        .filter_map(|entry| match &entry.kind {
            EntryKind::User { parts } => Some(parts),
            _ => None,
        })
        .flat_map(|parts| parts.iter())
        .any(|part| {
            matches!(
                part,
                JournalPart::Image { .. } | JournalPart::ImageBlob { .. }
            )
        }))
}

/// Waits until some journaled user entry carries image parts. The shared
/// snapshot behind `view` can lag the journal receipt that already fed the
/// request, so a single read can miss entries a poll observes; a product
/// that never journals the images times out with the entry dump instead.
async fn wait_journal_user_images(agent: &Agent, context: String) -> Outcome<()> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        if journal_user_images(agent)? {
            return Ok(());
        }
        if std::time::Instant::now() > deadline {
            let dump = agent
                .view(PageReq::default())?
                .entries
                .items
                .iter()
                .map(|entry| format!("{:?}", entry.kind))
                .collect::<Vec<_>>();
            return Err(format!(
                "no journaled user entry carried images; entries={dump:?}; {context}"
            )
            .into());
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Holds the first input it sees open until the opening is cancelled. It
/// sits after the letter hook, so the letter hook has already returned its
/// transform when this one parks.
struct Held {
    armed: Arc<AtomicBool>,
    entered: Arc<Notify>,
}

impl Hook<InputEvent, InputVerdict> for Held {
    fn call(
        &self,
        _input: InputEvent,
        cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let hold = self.armed.swap(false, Ordering::SeqCst);
        let entered = Arc::clone(&self.entered);
        let cancel = cx.cancel.clone();
        Box::pin(async move {
            if hold {
                entered.notify_one();
                cancel.cancelled().await;
            }
            Ok(InputVerdict::Continue)
        })
    }
}

fn held(armed: &Arc<AtomicBool>, entered: &Arc<Notify>) -> Outcome<Extension> {
    Ok(
        ExtensionBuilder::new("zz-held", "0.1.0", ServiceSet::default())?
            .on_input(Held {
                armed: Arc::clone(armed),
                entered: Arc::clone(entered),
            })
            .build()?,
    )
}

async fn wait_entered(entered: &Notify) -> Outcome<()> {
    tokio::time::timeout(WAIT, entered.notified())
        .await
        .map_err(|_| "the held hook never ran".into())
}

/// F1 (cancel): the hook persists its records, then a later opening hook is
/// cancelled. The opening drops the transform, the user entry keeps only the
/// original text, and the next input sees a full record set and attaches
/// nothing: records exist, the model never received an image.
#[tokio::test]
async fn letter_red_f1_cancelled_opening_leaves_records_the_model_never_received() -> Outcome<()> {
    let rig = Rig::new("red-f1-cancel")?;
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Notify::new());
    let host = rig
        .start_with(&[ALPHA, BETA], VISION_MODEL, vec![held(&armed, &entered)?])
        .await?;
    let agent = rig.open(&host, "red-f1-cancel").await?;
    let mut subscription = agent.subscribe(None)?;

    armed.store(true, Ordering::SeqCst);
    let turn = submit_prompt(&agent, "hello").await?;
    wait_entered(&entered).await?;
    agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await?;
    assert_eq!(drain_stop(&mut subscription, turn).await?, Stop::Cancelled);
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1), Some(2)],
        "premise: the hook persisted both records before the opening was cancelled"
    );
    assert!(
        !ext_rows(&rig)?
            .iter()
            .any(|(ext, kind)| { ext == "letter" && kind == "letter_delivered" }),
        "premise: the cancelled opening wrote no delivery marker"
    );

    let notes = prompt(&agent, &mut subscription, "after-cancel").await?;
    wait_request_with(&rig, "after-cancel").await?;
    assert_names_are_backed_by_images(&rig, &notes)?;
    wait_journal_user_images(
        &agent,
        format!(
            "notes={:?}; ext_rows={:?}",
            letter_notices(&notes),
            ext_rows(&rig)?
        ),
    )
    .await?;

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// F1 (shutdown): same window, closed by a host shutdown instead of a cancel.
#[tokio::test]
async fn letter_red_f1_shutdown_during_opening_leaves_records_the_model_never_received()
-> Outcome<()> {
    let rig = Rig::new("red-f1-shutdown")?;
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Notify::new());
    let host = rig
        .start_with(&[ALPHA, BETA], VISION_MODEL, vec![held(&armed, &entered)?])
        .await?;
    let agent = rig.open(&host, "red-f1-shutdown").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;

    armed.store(true, Ordering::SeqCst);
    submit_prompt(&agent, "hello").await?;
    wait_entered(&entered).await?;
    let drain = async {
        while tokio::time::timeout(WAIT, subscription.next())
            .await
            .ok()
            .flatten()
            .is_some()
        {}
    };
    let (_report, ()) = tokio::join!(host.shutdown(Duration::from_secs(10)), drain);
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1), Some(2)],
        "premise: the hook persisted both records before shutdown dropped the opening"
    );

    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "red-f1-shutdown-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;
    assert_names_are_backed_by_images(&rig, &notes)?;
    wait_journal_user_images(&resumed, String::new()).await?;

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// F2 (frozen mismatch): the loaded skills no longer match the stored set, so
/// the hook freezes and attaches nothing, yet the prompt still shows `beta`
/// and the changed `alpha` by name only.
#[tokio::test]
async fn letter_red_f2_frozen_mismatch_hides_descriptions_no_image_carries() -> Outcome<()> {
    let rig = Rig::new("red-f2-mismatch")?;
    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let agent = rig.open(&host, "red-f2-mismatch").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    let mut notes = prompt(&agent, &mut subscription, "hello").await?;
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1)],
        "premise: the first lettered turn stored one letter"
    );
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    let changed: Skill = ("alpha", "Review the diff with great care.", true);
    let host = rig.start(&[changed, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "red-f2-mismatch-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    notes.extend(prompt(&resumed, &mut subscription, "again").await?);
    assert_names_are_backed_by_images(&rig, &notes)?;

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// F2 (hook failure): a damaged letter row makes the hook fail on an input
/// that has delivered nothing, and the prompt still shows `alpha` by name.
#[tokio::test]
async fn letter_red_f2_failed_hook_keeps_names_only_though_nothing_was_delivered() -> Outcome<()> {
    let rig = Rig::new("red-f2-failure")?;
    let host = rig.start(&[ALPHA], TEXT_MODEL).await?;
    let agent = rig.open(&host, "red-f2-failure").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    let mut notes = prompt(&agent, &mut subscription, "seed").await?;
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    let damaged = dal_core::encode(&Record::Ext {
        at: Timestamp::now(),
        ext: "letter".into(),
        kind: "letter".into(),
        body: RawJson::parse(r#"{"v":1,"id":"1","kind":"skill","bogus":true}"#)?,
    })?;
    std::fs::OpenOptions::new()
        .append(true)
        .open(rig.journal_of(session)?)?
        .write_all(&damaged)?;

    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "red-f2-failure-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    notes.extend(prompt(&resumed, &mut subscription, "again").await?);
    assert!(
        letter_notices(&notes)
            .iter()
            .any(|notice| notice.starts_with("letter: skill letters were not persisted:")),
        "premise: the damaged row made the letter hook fail with its cause"
    );
    assert_names_are_backed_by_images(&rig, &notes)?;

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// Rewrites the persisted `letter` row for id `1` to version 2, the on-disk
/// state of a well-formed row the admitted schema no longer accepts. Returns
/// the replacement journal line, or `None` for any other line.
fn rewrite_letter_row_v2(record: Record) -> Outcome<Option<Vec<u8>>> {
    let Record::Ext { at, ext, kind, body } = record else {
        return Ok(None);
    };
    if ext.as_ref() != "letter" || kind.as_ref() != "letter" {
        return Ok(None);
    }
    let Ok(mut stored) = body.decode_as::<SkillLetterRecord>() else {
        return Ok(None);
    };
    if stored.id.as_ref() != "1" {
        return Ok(None);
    }
    assert_eq!(stored.v, 1, "premise: the persisted row uses version 1");
    stored.v = 2;
    let body = RawJson::parse(&sonic_rs::to_string(&stored)?)?;
    Ok(Some(dal_core::encode(
        &Record::Ext { at, ext, kind, body },
    )?))
}

/// D1: a well-formed persistent row with an unsupported version fails the
/// hook closed on resume. The corrupt row is preserved, no duplicate id is
/// appended, the failed turn delivers nothing, and the turn still ends with
/// the running text.
#[tokio::test]
async fn letter_unsupported_version_row_fails_closed_without_a_duplicate() -> Outcome<()> {
    let rig = Rig::new("invalid-version")?;
    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let agent = rig.open(&host, "invalid-version").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "seed").await?;
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1)],
        "premise: the opening persisted one valid row"
    );
    assert_eq!(
        delivery_markers(&rig)?,
        1,
        "premise: the opening delivered once"
    );
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    // Corrupt the persisted row in place: same id, unsupported version. The
    // host is shut down, so no actor owns the file.
    let path = rig.journal_of(session)?;
    let text = std::fs::read_to_string(&path)?;
    let mut rewritten = String::with_capacity(text.len());
    let mut changed = 0_usize;
    for line in text.split_inclusive('\n') {
        let probe = line.strip_suffix('\n').unwrap_or(line);
        let hit = match dal_core::decode(probe.as_bytes()) {
            Ok(decoded) => rewrite_letter_row_v2(decoded.record)?,
            Err(_) => None,
        };
        match hit {
            Some(bytes) => {
                rewritten.push_str(std::str::from_utf8(&bytes)?);
                changed += 1;
            }
            None => rewritten.push_str(line),
        }
    }
    assert_eq!(changed, 1, "exactly the persisted letter row was rewritten");
    std::fs::write(&path, rewritten)?;

    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "invalid-version-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;
    assert!(
        letter_notices(&notes).contains(
            &"letter: skill letters were not persisted: letter record '1' failed validation."
        ),
        "the invalid row fails the hook with its cause: {notes:?}"
    );
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![None],
        "no duplicate id is appended; the corrupt row is preserved"
    );
    assert_eq!(
        delivery_markers(&rig)?,
        1,
        "the failed turn delivers nothing"
    );

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

/// F3: the stored set is a delivered strict prefix and the registry gains a
/// marked skill. The hook freezes the delivered set: it persists no tail and
/// attaches no letter images, so the delivered letter is never sent twice.
#[tokio::test]
async fn letter_red_f3_registry_growth_does_not_redeliver_a_delivered_letter() -> Outcome<()> {
    let rig = Rig::new("red-f3-growth")?;
    let host = rig.start(&[ALPHA], VISION_MODEL).await?;
    let agent = rig.open(&host, "red-f3-growth").await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    prompt(&agent, &mut subscription, "hello").await?;
    assert_eq!(
        image_parts_in(&rig.requests()?[0].1),
        1,
        "premise: the first lettered turn delivered one letter"
    );
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;

    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "red-f3-growth-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    prompt(&resumed, &mut subscription, "again").await?;

    let requests = rig.requests()?;
    let delivered = image_parts_in(&requests[1].1);
    let stored = rig.letter_records()?.len();
    assert_eq!(
        delivered, stored,
        "every stored letter reaches the request exactly once: users={:?}",
        requests[1].1
    );

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

// ---------------------------------------------------------------------------
// Atomic capture and description visibility (`letter_atomic_*`).
//
// A crash can stop the hook between two record appends: the journal then holds
// a strict prefix of the set. The next input must finish the set without a
// duplicate or conflicting id, and a prompt line may show a skill by name only
// while the request holds the image that carries its description.
// ---------------------------------------------------------------------------

/// Removes the journal row of letter `id`, the on-disk state of a crash after
/// the earlier rows were durable and before this one was. The host must be
/// shut down: the file is edited while no actor owns it.
fn drop_letter_row(rig: &Rig, session: SessionId, id: &str) -> Outcome<()> {
    let path = rig.journal_of(session)?;
    let text = std::fs::read_to_string(&path)?;
    let mut kept = String::with_capacity(text.len());
    let mut dropped = 0_usize;
    for line in text.split_inclusive('\n') {
        let is_row = if line.trim().is_empty() {
            false
        } else {
            match dal_core::decode(line.as_bytes())?.record {
                Record::Ext {
                    ext, kind, body, ..
                } if ext.as_ref() == "letter" && kind.as_ref() == "letter" => {
                    body.decode_as::<SkillLetterRecord>()?.id.as_ref() == id
                }
                _ => false,
            }
        };
        if is_row {
            dropped += 1;
        } else {
            kept.push_str(line);
        }
    }
    assert_eq!(
        dropped, 1,
        "the journal holds exactly one row for letter {id}"
    );
    std::fs::write(&path, kept)?;
    Ok(())
}

/// Opens a session on an image model whose opening hook cancels after the
/// letter hook persisted both records, closes it, and removes the second
/// record: the journal of a crash between the two appends. Returns the
/// session; the rig's journal holds letter row `1` only.
async fn crashed_between_records(rig: &Rig, tag: &str) -> Outcome<SessionId> {
    let armed = Arc::new(AtomicBool::new(false));
    let entered = Arc::new(Notify::new());
    let host = rig
        .start_with(&[ALPHA, BETA], VISION_MODEL, vec![held(&armed, &entered)?])
        .await?;
    let agent = rig.open(&host, tag).await?;
    let session = agent.session();
    let mut subscription = agent.subscribe(None)?;
    armed.store(true, Ordering::SeqCst);
    let turn = submit_prompt(&agent, "hello").await?;
    wait_entered(&entered).await?;
    agent
        .submit(Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        })
        .await?;
    assert_eq!(drain_stop(&mut subscription, turn).await?, Stop::Cancelled);
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1), Some(2)],
        "premise: the hook persisted both records before the opening was cancelled"
    );
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    let _report = host.shutdown(GRACE).await;
    drop_letter_row(rig, session, "2")?;
    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1)],
        "premise: only the first record reached the disk"
    );
    Ok(session)
}

/// The index of the request whose user parts carry `text`.
fn request_with(rig: &Rig, text: &str) -> Outcome<usize> {
    rig.requests()?
        .iter()
        .position(|(_, users)| users.iter().flatten().any(|part| part.as_str() == text))
        .ok_or_else(|| format!("no request carried {text:?}").into())
}

/// How many delivery markers the journal holds.
fn delivery_markers(rig: &Rig) -> Outcome<usize> {
    Ok(ext_rows(rig)?
        .iter()
        .filter(|(ext, kind)| ext == "letter" && kind == "letter_delivered")
        .count())
}

#[tokio::test]
async fn letter_atomic_crashed_prefix_completes_to_one_contiguous_set() -> Outcome<()> {
    let rig = Rig::new("atomic-prefix")?;
    let session = crashed_between_records(&rig, "atomic-prefix").await?;

    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "atomic-prefix-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;

    let letters = rig.letter_records()?;
    assert_eq!(
        ids(&letters),
        vec![Some(1), Some(2)],
        "the missing tail takes the next id: no duplicate, no gap"
    );
    assert_eq!(
        letters
            .iter()
            .map(|record| &*record.skill)
            .collect::<Vec<_>>(),
        ["alpha", "beta"]
    );
    // The cancelled first turn sends a plain request before the cancel lands;
    // the resumed request is the one carrying the resumed prompt.
    let again = request_with(&rig, "text:again")?;
    let requests = rig.requests()?;
    assert_eq!(
        image_parts_in(&requests[again].1),
        2,
        "the completed set reaches the model whole, the persisted letter included: users={:?}",
        requests[again].1
    );
    assert_names_are_backed_by_images(&rig, &notes)?;
    assert_eq!(delivery_markers(&rig)?, 1, "one marker for one delivery");

    prompt(&resumed, &mut subscription, "third").await?;
    let third = request_with(&rig, "text:third")?;
    assert_eq!(
        image_parts_in(&rig.requests()?[third].1),
        2,
        "the delivered set stays in history and is not attached again"
    );
    assert_eq!(
        rig.letter_records()?.len(),
        2,
        "the delivered set is frozen"
    );
    assert_eq!(delivery_markers(&rig)?, 1);

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn letter_atomic_crashed_prefix_never_appends_after_the_skills_change() -> Outcome<()> {
    let rig = Rig::new("atomic-changed")?;
    let session = crashed_between_records(&rig, "atomic-changed").await?;

    let changed: Skill = ("alpha", "Review the diff with great care.", true);
    let host = rig.start(&[changed, BETA], VISION_MODEL).await?;
    let resumed = rig.resume(&host, session, "atomic-changed-resume").await?;
    let mut subscription = resumed.subscribe(None)?;
    let notes = prompt(&resumed, &mut subscription, "again").await?;

    assert_eq!(
        ids(&rig.letter_records()?),
        vec![Some(1)],
        "the prefix no longer matches the loaded skills: nothing is appended"
    );
    let again = request_with(&rig, "text:again")?;
    assert_eq!(
        image_parts_in(&rig.requests()?[again].1),
        0,
        "a frozen set that never reached the model attaches nothing"
    );
    assert_names_are_backed_by_images(&rig, &notes)?;
    let systems = rig.systems()?;
    let system = &systems[again];
    for line in [
        "- alpha: Review the diff with great care.",
        "- beta: Write a changelog entry.",
    ] {
        assert!(
            system.contains(line),
            "the description stays visible as text: {line:?} in {system:?}"
        );
    }

    let _report = host.shutdown(GRACE).await;
    Ok(())
}

#[tokio::test]
async fn letter_atomic_descriptions_return_to_text_when_the_model_stops_reading_images()
-> Outcome<()> {
    let rig = Rig::new("atomic-switch")?;
    let host = rig.start(&[ALPHA, BETA], VISION_MODEL).await?;
    let agent = rig.open(&host, "atomic-switch").await?;
    let mut subscription = agent.subscribe(None)?;

    prompt(&agent, &mut subscription, "hello").await?;
    assert_eq!(
        name_only_lines(&rig.systems()?[0]),
        2,
        "premise: the delivered letters carry both descriptions"
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

    let systems = rig.systems()?;
    assert_eq!(
        name_only_lines(&systems[1]),
        0,
        "a text model reads no image"
    );
    for line in [
        "- alpha: Review the diff carefully.",
        "- beta: Write a changelog entry.",
    ] {
        assert!(
            systems[1].contains(line),
            "the description is text for the text model: {line:?} in {:?}",
            systems[1]
        );
    }

    let _report = host.shutdown(GRACE).await;
    Ok(())
}
