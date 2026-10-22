//! Input hook transformations reach the turn they open.
//!
//! A real host, store, and session actor run every case. The public synthetic
//! model records the provider request it receives together with the journal
//! bytes already on disk when it does, so a transformation that only reaches
//! a hook-local value, or a record written after the request, fails here.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test helpers fail loudly at their boundary")]
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use dal_agent::ext::{
    BoxFuture, EventStream, Extension, ExtensionBuilder, Hook, HookCx, HookError, ModelCx,
    ModelError, ModelHandler, ModelRecord,
};
use dal_agent::{Agent, Delivery, Env, Host, Product, SessionRef, Subscription};
use dal_core::ext::{BeforeTurn, InputEvent, InputVerdict};
use dal_core::{
    Caps, ClientId, Command, Config, ConfigProduct, ContextItem, EntryKind, Expect, JournalPart,
    ModelId, ModelRequest, Origin, Part, Record, Reply, ServiceSet, Stop, ThinkingLevel, TurnId,
    UpdateKind, Usage, Workspace,
};
use dal_provider::{ProviderError, StopReason, StreamEvent};
use tokio::sync::Notify;

/// One scripted provider request: a complete text stream ending the turn.
const REPLY_STEP: &str = "{\"kind\":\"events\",\"events\":[{\"type\":\"text_delta\",\"text\":\"ok\"},{\"type\":\"tool_calls_done\",\"calls\":[]},{\"type\":\"usage\",\"usage\":{\"input_tokens\":10,\"cached_input_tokens\":0,\"output_tokens\":5,\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}},{\"type\":\"stop\",\"reason\":\"end_turn\"}]}\n";

/// A few image bytes; base64 of them holds no character a journal escapes.
const IMAGE: [u8; 12] = [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3, 4];

/// Text a cancelled opening must never admit.
const MARKER: &str = "TRANSFORMED-MARKER";

fn text(value: &str) -> Part {
    Part::Text { text: value.into() }
}

fn image() -> Part {
    Part::Image {
        mime: "image/png".into(),
        bytes: IMAGE.to_vec().into(),
    }
}

/// The journal spelling of `parts`, written independently of the fold.
fn journaled(parts: &[Part]) -> Vec<JournalPart> {
    parts
        .iter()
        .map(|part| match part {
            Part::Text { text } => JournalPart::Text { text: text.clone() },
            Part::Image { mime, bytes } => JournalPart::Image {
                mime: mime.clone(),
                base64: STANDARD.encode(bytes).into(),
            },
            Part::Blob { .. } => panic!("test content is text or image, got {part:?}"),
        })
        .collect()
}

/// Replaces the text parts with `edit` of their join and keeps the other
/// parts after it, the way a text-only hook rewrites mixed content.
fn rewrite_text(content: &[Part], edit: impl FnOnce(&str) -> String) -> Vec<Part> {
    let joined: String = content
        .iter()
        .filter_map(|part| match part {
            Part::Text { text } => Some(text.as_ref()),
            _ => None,
        })
        .collect();
    let rest = content
        .iter()
        .filter(|part| !matches!(part, Part::Text { .. }))
        .cloned();
    std::iter::once(Part::Text {
        text: edit(&joined).into(),
    })
    .chain(rest)
    .collect()
}

fn user_parts(kind: &EntryKind) -> Option<Vec<JournalPart>> {
    match kind {
        EntryKind::User { parts } => Some(parts.clone()),
        _ => None,
    }
}

fn accepted(reply: &Reply) -> TurnId {
    match reply {
        Reply::Accepted { turn, .. } => *turn,
        other => panic!("the prompt was not accepted: {other:?}"),
    }
}

/// Every readable file under `root`, joined: the session journal among them.
fn disk_text(root: &Path) -> String {
    let mut text = String::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for path in entries.flatten().map(|entry| entry.path()) {
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(content) = std::fs::read_to_string(&path) {
                text.push_str(&content);
            }
        }
    }
    text
}

/// One provider request as the synthetic model received it.
#[derive(Clone)]
struct Seen {
    /// The user messages of the request context, in order.
    users: Vec<Vec<Part>>,
    /// The file bytes on disk when the request arrived.
    disk: String,
}

/// Holds the first provider request until the test releases it.
struct Hold {
    armed: AtomicBool,
    entered: Notify,
    gate: Notify,
}

/// A public synthetic model that records every request it receives.
struct Recorder {
    data: PathBuf,
    requests: Arc<Mutex<Vec<Seen>>>,
    hold: Option<Arc<Hold>>,
}

fn user_messages(request: &ModelRequest) -> Vec<Vec<Part>> {
    request
        .context
        .iter()
        .filter_map(|item| match item {
            ContextItem::User { parts } => Some(parts.clone()),
            _ => None,
        })
        .collect()
}

fn reply() -> Vec<Result<StreamEvent, ProviderError>> {
    vec![
        Ok(StreamEvent::TextDelta { text: "ok".into() }),
        Ok(StreamEvent::ToolCallsDone { calls: Vec::new() }),
        Ok(StreamEvent::Usage {
            usage: Usage {
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
    ]
}

impl ModelHandler for Recorder {
    fn run<'a>(
        &'a self,
        request: ModelRequest,
        _cx: ModelCx<'a>,
    ) -> BoxFuture<'a, Result<EventStream, ModelError>> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(Seen {
                users: user_messages(&request),
                disk: disk_text(&self.data),
            });
        let hold = self.hold.clone();
        Box::pin(async move {
            if let Some(hold) = hold
                && hold.armed.swap(false, Ordering::SeqCst)
            {
                hold.entered.notify_one();
                hold.gate.notified().await;
            }
            Ok(EventStream::new(futures::stream::iter(reply()), || {}))
        })
    }
}

type Inputs = Arc<Mutex<Vec<Vec<Part>>>>;
type ProbeAnswer = Box<dyn Fn(&[Part]) -> InputVerdict + Send + Sync>;

/// An input hook that answers with `answer` and records the content it saw.
struct InputProbe {
    answer: ProbeAnswer,
    inputs: Inputs,
}

impl Hook<InputEvent, InputVerdict> for InputProbe {
    fn call(
        &self,
        input: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let verdict = (self.answer)(&input.content);
        self.inputs
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(input.content);
        Box::pin(async move { Ok(verdict) })
    }
}

fn probe(answer: impl Fn(&[Part]) -> InputVerdict + Send + Sync + 'static) -> (InputProbe, Inputs) {
    let inputs = Inputs::default();
    let hook = InputProbe {
        answer: Box::new(answer),
        inputs: Arc::clone(&inputs),
    };
    (hook, inputs)
}

fn recorded(inputs: &Inputs) -> Vec<Vec<Part>> {
    inputs
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .clone()
}

/// An input hook that stays in flight until its turn is cancelled.
struct HeldInput {
    entered: Arc<Notify>,
}

impl Hook<InputEvent, InputVerdict> for HeldInput {
    fn call(
        &self,
        _input: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        let entered = Arc::clone(&self.entered);
        Box::pin(async move {
            entered.notify_one();
            std::future::pending::<()>().await;
            Ok(InputVerdict::Continue)
        })
    }
}

/// A `before_turn` hook that records the text it is given.
struct TextProbe {
    texts: Arc<Mutex<Vec<String>>>,
}

impl Hook<BeforeTurn, Option<String>> for TextProbe {
    fn call(
        &self,
        input: BeforeTurn,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<Option<String>, HookError>> {
        self.texts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(input.text.to_string());
        Box::pin(async { Ok(None) })
    }
}

fn extension(name: &str, origin: Origin) -> ExtensionBuilder {
    ExtensionBuilder::new(name, "0.1.0", ServiceSet::default())
        .expect("builder")
        .with_origin(origin, None)
}

/// A real host with one open session and one subscription.
struct Harness {
    host: Host,
    workspace: Workspace,
    data: PathBuf,
    agent: Agent,
    subscription: Subscription,
    requests: Arc<Mutex<Vec<Seen>>>,
    _tmp: tempfile::TempDir,
}

fn dirs() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let tmp = tempfile::tempdir().expect("tempdir");
    let data = tmp.path().join("data");
    let work = tmp.path().join("w");
    std::fs::create_dir_all(&data).expect("data dir");
    std::fs::create_dir_all(&work).expect("workspace dir");
    (tmp, data, work)
}

impl Harness {
    /// Starts a host whose model is the recording synthetic model.
    async fn synthetic(hold: Option<Arc<Hold>>, hooks: Vec<Extension>) -> Self {
        let (tmp, data, work) = dirs();
        let requests = Arc::new(Mutex::new(Vec::new()));
        let model = extension("model", Origin::Builtin)
            .model(ModelRecord {
                id: ModelId::parse("fixture/entry").expect("model id"),
                caps: Caps {
                    context_window: Some(100_000),
                    thinking: Box::new([ThinkingLevel::Off]),
                    tool_use: false,
                    image_input: true,
                    custom_grammar: false,
                },
                handler: Arc::new(Recorder {
                    data: data.clone(),
                    requests: Arc::clone(&requests),
                    hold,
                }),
                export: None,
            })
            .build()
            .expect("model extension");
        let mut extensions = vec![model];
        extensions.extend(hooks);
        let config = String::from("model = \"fixture/entry\"\n");
        Self::start((tmp, data, work), &config, extensions, requests).await
    }

    /// Starts a host whose model is the scripted fixture provider.
    async fn scripted(hooks: Vec<Extension>) -> Self {
        let (tmp, data, work) = dirs();
        let fixture = data.join("script.jsonl");
        std::fs::write(&fixture, REPLY_STEP).expect("fixture");
        let config = format!(
            "model = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
            fixture.to_string_lossy().replace('\\', "\\\\")
        );
        Self::start((tmp, data, work), &config, hooks, Arc::default()).await
    }

    async fn start(
        dirs: (tempfile::TempDir, PathBuf, PathBuf),
        config: &str,
        extensions: Vec<Extension>,
        requests: Arc<Mutex<Vec<Seen>>>,
    ) -> Self {
        let (tmp, data, work) = dirs;
        let config = Config::load(ConfigProduct::Dalgon, &data, "", Some(config)).expect("config");
        let product = Product {
            name: "dal",
            data_root: data.clone(),
            defaults: "",
            extensions,
            bundled: Vec::new(),
        };
        let env = Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: work.clone(),
            sandbox_helper: None,
        };
        let host = Host::start(product, config, env).await.expect("host");
        let workspace = Workspace::new(work).expect("workspace");
        let agent = host
            .open(
                SessionRef::New {
                    workspace: workspace.clone(),
                    name: None,
                },
                ClientId::new("probe"),
            )
            .await
            .expect("open");
        let subscription = agent.subscribe(None).expect("subscribe");
        Self {
            host,
            workspace,
            data,
            agent,
            subscription,
            requests,
            _tmp: tmp,
        }
    }

    async fn prompt(&self, content: Vec<Part>) -> TurnId {
        let command = Command::Prompt {
            expect: Expect::Idle,
            content,
        };
        accepted(&self.agent.submit(command).await.expect("submit"))
    }

    /// Waits for the end of `turn` and returns its stop.
    async fn ended(&mut self, turn: TurnId) -> Stop {
        let wait = async {
            loop {
                let delivery = self.subscription.next().await.expect("stream open");
                let Delivery::Update(update) = delivery else {
                    continue;
                };
                if let UpdateKind::TurnEnded { turn: seen, stop } = &update.kind
                    && *seen == turn
                {
                    return *stop;
                }
            }
        };
        tokio::time::timeout(Duration::from_secs(30), wait)
            .await
            .expect("the turn ends in time")
    }

    fn requests(&self) -> Vec<Seen> {
        self.requests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// The user entries of the live view.
    fn view_users(&self) -> Vec<Vec<JournalPart>> {
        self.agent
            .view(dal_core::PageReq::default())
            .expect("view")
            .entries
            .items
            .iter()
            .filter_map(|entry| user_parts(&entry.kind))
            .collect()
    }

    /// The user records the store holds after the session closes.
    async fn durable_users(&self) -> Vec<Vec<JournalPart>> {
        let session = self.agent.session();
        self.host.close(session).await.expect("close the session");
        let store = dal_store::Store::new(
            self.data.clone(),
            self.workspace.clone(),
            dal_core::Product::Dal,
        );
        let (journal, _) = store
            .open_session(session)
            .await
            .expect("reopen the durable journal");
        journal
            .records()
            .iter()
            .filter_map(|record| match record {
                Record::User(entry) => user_parts(&entry.kind),
                _ => None,
            })
            .collect()
    }
}

/// Two chained hooks rewrite the text and append an image. The provider
/// request, the live entry, and the durable record all carry the final
/// parts, the later hook folds over the earlier hook's output, and the
/// journal already holds the final parts when the request arrives.
#[tokio::test]
async fn chained_text_and_image_transforms_reach_the_request_and_the_user_record() {
    let (first, first_inputs) = probe(|content| {
        InputVerdict::Transform(rewrite_text(content, |text| format!("{text}-one")))
    });
    let (second, second_inputs) = probe(|content| {
        let mut parts = rewrite_text(content, |text| format!("{text}+two"));
        parts.push(image());
        InputVerdict::Transform(parts)
    });
    let before_texts = Arc::new(Mutex::new(Vec::new()));
    let chain = extension("chain", Origin::Builtin)
        .on_input(first)
        .on_input(second)
        .on_before_turn(TextProbe {
            texts: Arc::clone(&before_texts),
        })
        .build()
        .expect("chain extension");
    let mut live = Harness::synthetic(None, vec![chain]).await;
    let turn = live.prompt(vec![text("original")]).await;
    assert_eq!(live.ended(turn).await, Stop::EndTurn);
    let expected = vec![text("original-one+two"), image()];
    assert_eq!(recorded(&first_inputs), vec![vec![text("original")]]);
    assert_eq!(recorded(&second_inputs), vec![vec![text("original-one")]]);
    let requests = live.requests();
    assert_eq!(requests.len(), 1, "one provider request");
    assert_eq!(
        requests[0].users,
        vec![expected.clone()],
        "the provider request carries the final parts"
    );
    assert!(
        requests[0].disk.contains("original-one+two")
            && requests[0].disk.contains(&STANDARD.encode(IMAGE)),
        "the final parts are on disk before the provider request"
    );
    assert_eq!(live.view_users(), vec![journaled(&expected)]);
    {
        let texts = before_texts.lock().unwrap_or_else(PoisonError::into_inner);
        assert_eq!(*texts, vec!["original-one+two".to_owned()]);
    }
    assert_eq!(live.durable_users().await, vec![journaled(&expected)]);
}

/// Hooks that continue leave the admitted parts, image included, exactly as
/// submitted in the request and in the record.
#[tokio::test]
async fn continue_keeps_the_admitted_parts_exactly() {
    let (hook, inputs) = probe(|_| InputVerdict::Continue);
    let chain = extension("chain", Origin::Builtin)
        .on_input(hook)
        .build()
        .expect("chain extension");
    let mut live = Harness::synthetic(None, vec![chain]).await;
    let admitted = vec![text("plain"), image()];
    let turn = live.prompt(admitted.clone()).await;
    assert_eq!(live.ended(turn).await, Stop::EndTurn);
    assert_eq!(recorded(&inputs), vec![admitted.clone()]);
    assert_eq!(live.requests()[0].users, vec![admitted.clone()]);
    assert_eq!(live.view_users(), vec![journaled(&admitted)]);
    assert_eq!(live.durable_users().await, vec![journaled(&admitted)]);
}

/// A handled result from a built-in extension lets later extensions run; the
/// first handled result from a later origin ends the fold. The content at
/// that point, and nothing a skipped hook would add, is admitted.
#[tokio::test]
async fn a_stopping_hook_ends_the_fold_and_its_content_is_admitted() {
    let (seed, _) =
        probe(|content| InputVerdict::Transform(rewrite_text(content, |_| "seed".to_owned())));
    let (quiet, _) = probe(|_| InputVerdict::Handled);
    let (append, append_inputs) = probe(|content| {
        InputVerdict::Transform(rewrite_text(content, |text| format!("{text}+stopper")))
    });
    let (stop, _) = probe(|_| InputVerdict::Handled);
    let (never, never_inputs) =
        probe(|content| InputVerdict::Transform(rewrite_text(content, |_| "never".to_owned())));
    let hooks: Vec<Extension> = vec![
        extension("seed", Origin::Builtin).on_input(seed),
        extension("quiet", Origin::Builtin).on_input(quiet),
        extension("stopper", Origin::User)
            .on_input(append)
            .on_input(stop),
        extension("zlate", Origin::User).on_input(never),
    ]
    .into_iter()
    .map(|builder| builder.build().expect("hook extension"))
    .collect();
    let mut live = Harness::synthetic(None, hooks).await;
    let turn = live.prompt(vec![text("original")]).await;
    assert_eq!(live.ended(turn).await, Stop::EndTurn);
    assert_eq!(
        recorded(&append_inputs),
        vec![vec![text("seed")]],
        "a built-in handled result did not stop the fold"
    );
    assert!(recorded(&never_inputs).is_empty(), "the fold stopped");
    let expected = vec![text("seed+stopper")];
    assert_eq!(live.requests()[0].users, vec![expected.clone()]);
    assert_eq!(live.durable_users().await, vec![journaled(&expected)]);
}

/// An input hook that fails.
struct FailingInput;

impl Hook<InputEvent, InputVerdict> for FailingInput {
    fn call(
        &self,
        _input: InputEvent,
        _cx: HookCx,
    ) -> BoxFuture<'static, Result<InputVerdict, HookError>> {
        Box::pin(async {
            Err(HookError::Failed {
                message: "the hook broke".into(),
            })
        })
    }
}

/// A failing hook between two transforming hooks keeps the running content:
/// the later hook still runs on the earlier hook's output, the turn admits
/// the final content, and exactly one notice names the failed hook.
#[tokio::test]
async fn a_failing_hook_keeps_the_running_content_and_raises_one_notice() {
    let (first, first_inputs) =
        probe(|content| InputVerdict::Transform(rewrite_text(content, |text| format!("{text}-a"))));
    let (last, last_inputs) =
        probe(|content| InputVerdict::Transform(rewrite_text(content, |text| format!("{text}-c"))));
    let chain = extension("chain", Origin::Builtin)
        .on_input(first)
        .on_input(FailingInput)
        .on_input(last)
        .build()
        .expect("chain extension");
    let mut live = Harness::synthetic(None, vec![chain]).await;
    let turn = live.prompt(vec![text("original")]).await;
    let mut notices = Vec::new();
    let stop = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            let delivery = live.subscription.next().await.expect("stream open");
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::Notice(notice) => notices.push(notice.text.to_string()),
                UpdateKind::TurnEnded { turn: seen, stop } if *seen == turn => return *stop,
                _ => {}
            }
        }
    })
    .await
    .expect("the turn ends in time");
    assert_eq!(stop, Stop::EndTurn);
    assert_eq!(recorded(&first_inputs), vec![vec![text("original")]]);
    assert_eq!(
        recorded(&last_inputs),
        vec![vec![text("original-a")]],
        "the hook after the failure still runs on the earlier transform"
    );
    let expected = vec![text("original-a-c")];
    assert_eq!(live.requests()[0].users, vec![expected.clone()]);
    assert_eq!(live.durable_users().await, vec![journaled(&expected)]);
    assert_eq!(
        notices,
        vec![r#"plugin "chain" hook input failed; input continued unchanged"#.to_owned()],
        "one notice names the failed hook"
    );
}

/// A cancel that lands while a later hook is held ends the opening. The
/// transform an earlier hook already returned is not admitted: the turn
/// journals the content as submitted, and no request or record carries the
/// transformed text.
#[tokio::test]
async fn cancelling_while_a_later_hook_is_held_admits_the_original_content() {
    let (early, _) =
        probe(|content| InputVerdict::Transform(rewrite_text(content, |_| MARKER.to_owned())));
    let entered = Arc::new(Notify::new());
    let chain = extension("chain", Origin::Builtin)
        .on_input(early)
        .on_input(HeldInput {
            entered: Arc::clone(&entered),
        })
        .build()
        .expect("chain extension");
    let mut live = Harness::synthetic(None, vec![chain]).await;
    let turn = live.prompt(vec![text("original-cancel")]).await;
    tokio::time::timeout(Duration::from_secs(30), entered.notified())
        .await
        .expect("the held hook is entered");
    let cancel = Command::Cancel {
        scope: dal_core::CancelScope::Turn(turn),
    };
    let reply = live.agent.submit(cancel).await.expect("cancel");
    assert!(
        matches!(reply, Reply::Done(_)),
        "cancel resolves: {reply:?}"
    );
    assert_eq!(live.ended(turn).await, Stop::Cancelled);
    let expected = vec![journaled(&[text("original-cancel")])];
    assert_eq!(live.view_users(), expected);
    for seen in live.requests() {
        assert!(
            !seen.disk.contains(MARKER) && !format!("{:?}", seen.users).contains(MARKER),
            "no request saw the transformed text"
        );
    }
    assert_eq!(live.durable_users().await, expected);
}

/// A follow-up opens from the settled turn, runs the input hooks, and
/// carries the transformed content into its request and record while the
/// first turn keeps its own.
#[tokio::test]
async fn a_follow_up_opened_after_a_turn_is_transformed() {
    let (rewrite, inputs) = probe(|content| {
        InputVerdict::Transform(rewrite_text(content, |text| format!("rewritten:{text}")))
    });
    let chain = extension("chain", Origin::Builtin)
        .on_input(rewrite)
        .build()
        .expect("chain extension");
    let hold = Arc::new(Hold {
        armed: AtomicBool::new(true),
        entered: Notify::new(),
        gate: Notify::new(),
    });
    let mut live = Harness::synthetic(Some(Arc::clone(&hold)), vec![chain]).await;
    let first = live.prompt(vec![text("first")]).await;
    tokio::time::timeout(Duration::from_secs(30), hold.entered.notified())
        .await
        .expect("the first request reaches the model");
    let queued = Command::FollowUp {
        turn: first,
        content: vec![text("second")],
    };
    let second = match live
        .agent
        .submit(queued)
        .await
        .expect("queue the follow-up")
    {
        Reply::Queued { turn: Some(turn) } => turn,
        other => panic!("the follow-up was not queued: {other:?}"),
    };
    hold.gate.notify_one();
    assert_eq!(live.ended(first).await, Stop::EndTurn);
    assert_eq!(live.ended(second).await, Stop::EndTurn);
    assert_eq!(
        recorded(&inputs),
        vec![vec![text("first")], vec![text("second")]]
    );
    let first_final = vec![text("rewritten:first")];
    let second_final = vec![text("rewritten:second")];
    let requests = live.requests();
    assert_eq!(requests.len(), 2, "one provider request per turn");
    assert_eq!(requests[0].users, vec![first_final.clone()]);
    assert_eq!(
        requests[1].users,
        vec![first_final.clone(), second_final.clone()]
    );
    let expected = vec![journaled(&first_final), journaled(&second_final)];
    assert_eq!(live.view_users(), expected);
    assert_eq!(live.durable_users().await, expected);
}

/// The scripted fixture provider turn journals the transformed parts, so the
/// production provider path reads the same entry the synthetic model does.
#[tokio::test]
async fn a_scripted_provider_turn_journals_the_transformed_parts() {
    let (hook, _) = probe(|content| {
        let mut parts = rewrite_text(content, |text| format!("{text}-scripted"));
        parts.push(image());
        InputVerdict::Transform(parts)
    });
    let chain = extension("chain", Origin::Builtin)
        .on_input(hook)
        .build()
        .expect("chain extension");
    let mut live = Harness::scripted(vec![chain]).await;
    let turn = live.prompt(vec![text("original")]).await;
    assert_eq!(live.ended(turn).await, Stop::EndTurn);
    let expected = vec![journaled(&[text("original-scripted"), image()])];
    assert_eq!(live.view_users(), expected);
    assert_eq!(live.durable_users().await, expected);
}
