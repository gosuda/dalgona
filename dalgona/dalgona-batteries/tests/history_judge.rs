// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The history task opens its judge from the session setup identity,
//! submits each selected letter's stored text, and fails in the way the
//! park policy requires when that text cannot be submitted.
//!
//! A real resumed `Host` is seeded with thirty durable compaction letters
//! over distinct journal text, then runs ordinary turns against a loopback
//! model endpoint that records every request body. The settled hook reaches
//! auto-dream. Success requires the task's setup-captured `[judge]`
//! configuration and session model route to make the summary request,
//! rather than silently taking the gate-off path from an empty `JudgeOpen`
//! identity, and the recorded judge request must carry every selected
//! letter's exact stored text, oldest first and labeled by id.
//!
//! Three boundary scenarios pin what happens when the whole batch cannot be
//! submitted: only the oldest-first prefix that fits the shared budget is
//! consumed; an oldest letter that cannot fit, and a letter whose text
//! cannot be resolved, make no judge call, consume nothing, and park on the
//! third identical failure rather than the sixth transient one.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::num::NonZeroU64;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use dal_agent::ext::grants::{GrantKey, GrantStore};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect, JournalPart, Part,
    RawJson, Record, SessionId, Stop, Timestamp, UpdateKind, Workspace,
};
use dal_ext::judge::SHARED_MAX;
use dal_store::Store;
use dalgona_batteries::history::{HistoryConfig, history};
use serde::Deserialize;
use sonic_rs::{JsonContainerTrait, JsonValueTrait};
use tokio::fs;
use tokio::net::TcpListener;

const MODEL: &str = "openai/gpt-6-luna";
const LETTERS: usize = 30;
const DREAM_SUMMARY: &str = "the captured history summary";
const WAIT: Duration = Duration::from_secs(30);
/// A source entry id no seeded journal contains.
const DANGLING_ENTRY: usize = 9_999;
/// Source bytes of the oversized oldest letter: past the shared budget alone.
const OVERSIZED: usize = SHARED_MAX + 1;
/// Per-letter padding that makes the whole batch exceed the shared budget.
const PADDING: usize = 1_000;
const FAILED_PREFIX: &str = "Auto-dream failed;";
const PARKED_PREFIX: &str = "Auto-dream paused after repeated failures";

type TestResult = Result<(), Box<dyn StdError>>;

#[derive(Deserialize)]
struct DreamBody {
    id: String,
    letters: Vec<String>,
    summary: String,
}

fn entry_id(value: usize) -> EntryId {
    let value = u64::try_from(value).expect("entry id fits u64");
    EntryId::new(NonZeroU64::new(value).expect("entry id is nonzero"))
}

fn user_record() -> Record {
    Record::User(Entry {
        id: entry_id(1),
        parent: None,
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text {
                text: "seeded history session".into(),
            }],
        },
    })
}

/// The distinct stored text behind compaction letter `index`.
fn letter_text_body(index: usize) -> String {
    format!("dream source {index} of thirty distinct entries")
}

/// The seeded journal: one source text per letter, oldest first.
struct Seed {
    texts: Vec<String>,
    /// One-based letter whose span names an entry absent from the branch.
    dangling: Option<usize>,
}

impl Seed {
    /// Thirty short distinct letters that fit the shared budget together.
    fn uniform() -> Self {
        Self {
            texts: (1..=LETTERS).map(letter_text_body).collect(),
            dangling: None,
        }
    }
}

fn source_record(index: usize, text: &str) -> Record {
    Record::User(Entry {
        id: entry_id(index + 1),
        parent: Some(entry_id(index)),
        at: jiff::Timestamp::UNIX_EPOCH,
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn compaction_record(index: usize, text_len: usize, entry: usize) -> Record {
    let body = format!(
        r#"{{"v":1,"id":"history/1.{index}","kind":"compaction","png_blob":"0000000000000000000000000000000000000000000000000000000000000000","png_bytes":1,"width":1,"height":1,"cell":[1,1],"spans":[[{entry},0,0,{text_len}]],"letters":[]}}"#
    );
    Record::Ext {
        at: jiff::Timestamp::UNIX_EPOCH,
        ext: "history".into(),
        kind: "letter".into(),
        body: RawJson::parse(&body).expect("letter body parses"),
    }
}

async fn seed_session(data: &Path, workspace: &Workspace, session: SessionId, seed: &Seed) {
    let store = Store::new(
        data.to_owned(),
        workspace.clone(),
        dal_core::Product::Dalgona,
    );
    let mut journal = store.create_session(session);
    let mut sources = vec![user_record()];
    sources.extend(
        seed.texts
            .iter()
            .enumerate()
            .map(|(offset, text)| source_record(offset + 1, text)),
    );
    journal.append(sources).await.expect("seed users");
    let letters: Vec<Record> = seed
        .texts
        .iter()
        .enumerate()
        .map(|(offset, text)| {
            let index = offset + 1;
            let entry = if seed.dangling == Some(index) {
                DANGLING_ENTRY
            } else {
                index + 1
            };
            compaction_record(index, text.len(), entry)
        })
        .collect();
    journal.append(letters).await.expect("seed history letters");
    journal.close().await.expect("close seeded journal");
}

/// Bodies the loopback model endpoint received, in arrival order.
struct CapturedBodies {
    bodies: Mutex<Vec<String>>,
}

/// Serves one model request with the canned dream summary and records the
/// exact bytes the product submitted.
async fn serve_canned(stream: tokio::net::TcpStream, captured: Arc<CapturedBodies>) -> TestResult {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
    let mut reader = BufReader::new(stream);
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    let mut content_length = 0usize;
    loop {
        line.clear();
        reader.read_line(&mut line).await?;
        let header = line.trim_end();
        if header.is_empty() {
            break;
        }
        if let Some((name, value)) = header.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0);
        }
    }
    let mut body = vec![0u8; content_length];
    if content_length > 0 {
        reader.read_exact(&mut body).await?;
    }
    captured
        .bodies
        .lock()
        .expect("bodies lock is not poisoned")
        .push(String::from_utf8_lossy(&body).into_owned());
    let stream_body = format!(
        "data: {{\"choices\":[{{\"delta\":{{\"content\":\"{DREAM_SUMMARY}\"}}}}]}}\n\ndata: {{\"choices\":[{{\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
    );
    let response = format!(
        "HTTP/1.1 200 OK\r\ncontent-type: text/event-stream\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{stream_body}",
        stream_body.len()
    );
    reader.into_inner().write_all(response.as_bytes()).await?;
    Ok(())
}

/// Submits one ordinary prompt and returns the first auto-dream notice the
/// settled history task sends after it.
async fn await_notice(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
) -> Result<String, Box<dyn StdError>> {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "trigger the settled history task".into(),
            }],
        })
        .await?;
    let notice = tokio::time::timeout(WAIT, async {
        while let Some(delivery) = subscription.next().await {
            let Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    panic!(
                        "pre-provisioned history grants must not open a request: {:?}",
                        request.question
                    );
                }
                UpdateKind::Notice(notice) if notice.kind.as_ref() == "history" => {
                    let text: &str = notice.text.as_ref();
                    if text.starts_with("Auto-dream ") {
                        return Ok::<String, Box<dyn StdError>>(text.to_owned());
                    }
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn, "trigger turn succeeds");
                }
                _ => {}
            }
        }
        Err("subscription closed before an auto-dream outcome".into())
    })
    .await
    .map_err(|_| "history judge regression timed out")??;
    Ok(notice)
}

/// Runs `rounds` trigger prompts in turn, one auto-dream notice each.
async fn trigger_rounds(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
    rounds: usize,
) -> Result<Vec<String>, Box<dyn StdError>> {
    let mut notices = Vec::with_capacity(rounds);
    for _ in 0..rounds {
        notices.push(await_notice(agent, subscription).await?);
    }
    Ok(notices)
}

fn dream_records(records: &[Record]) -> Vec<DreamBody> {
    records
        .iter()
        .filter_map(|record| {
            let Record::Ext {
                ext, kind, body, ..
            } = record
            else {
                return None;
            };
            if ext.as_ref() != "history" || kind.as_ref() != "letter" {
                return None;
            }
            let value = sonic_rs::from_str::<sonic_rs::Value>(body.as_str()).expect("letter body");
            if value.get("kind").and_then(JsonValueTrait::as_str) != Some("dream") {
                return None;
            }
            Some(sonic_rs::from_str::<DreamBody>(body.as_str()).expect("dream body"))
        })
        .collect()
}

/// What one scenario left behind.
struct Observed {
    /// The auto-dream notice after each trigger prompt, in order.
    notices: Vec<String>,
    /// Every request body the model endpoint received.
    bodies: Vec<String>,
    /// The dream records committed to the journal.
    dreams: Vec<DreamBody>,
}

/// The loopback model endpoint owning its listener and handler tasks in
/// structured `JoinSet`s; handler failures return as `String` so the task
/// outputs stay `Send`.
async fn spawn_judge_loopback() -> Result<
    (
        u16,
        Arc<CapturedBodies>,
        tokio::task::JoinSet<Result<(), String>>,
    ),
    Box<dyn StdError>,
> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();
    let captured = Arc::new(CapturedBodies {
        bodies: Mutex::new(Vec::new()),
    });
    let mut servers = tokio::task::JoinSet::new();
    servers.spawn({
        let captured = Arc::clone(&captured);
        async move {
            let mut handlers: tokio::task::JoinSet<Result<(), String>> =
                tokio::task::JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((stream, _)) = accepted else {
                            break;
                        };
                        let captured = Arc::clone(&captured);
                        handlers.spawn(async move {
                            serve_canned(stream, captured)
                                .await
                                .map_err(|error| error.to_string())
                        });
                    }
                    joined = handlers.join_next(), if !handlers.is_empty() => {
                        match joined {
                            Some(Err(error)) => return Err(error.to_string()),
                            Some(Ok(Err(message))) => return Err(message),
                            Some(Ok(Ok(()))) | None => {}
                        }
                    }
                }
            }
            while let Some(joined) = handlers.join_next().await {
                match joined {
                    Err(error) => return Err(error.to_string()),
                    Ok(Err(message)) => return Err(message),
                    Ok(Ok(())) => {}
                }
            }
            Ok(())
        }
    });
    Ok((port, captured, servers))
}

/// Shuts the loopback endpoint down, propagating handler failures.
async fn shutdown_judge_loopback(
    servers: &mut tokio::task::JoinSet<Result<(), String>>,
) -> Result<(), Box<dyn StdError>> {
    servers.abort_all();
    while let Some(joined) = servers.join_next().await {
        match joined {
            Err(error) if error.is_cancelled() => {}
            Err(error) => return Err(error.to_string().into()),
            Ok(Err(message)) => return Err(message.into()),
            Ok(Ok(())) => {}
        }
    }
    Ok(())
}

/// Seeds a session, runs `rounds` trigger prompts through a real `Host`
/// against the loopback model endpoint, and reads the journal back.
async fn run_scenario(seed: &Seed, rounds: usize) -> Result<Observed, Box<dyn StdError>> {
    let root = std::env::temp_dir().join(format!("dalgona-history-judge-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let workspace_dir = root.join("workspace");
    fs::create_dir_all(&data).await?;
    fs::create_dir_all(&workspace_dir).await?;
    let workspace = Workspace::new(workspace_dir.clone())?;
    let session = SessionId::new_v7();
    seed_session(&data, &workspace, session, seed).await;

    let (port, captured, mut servers) = spawn_judge_loopback().await?;
    let config_text = format!(
        "approval = \"all\"\nmodel = \"{MODEL}\"\n\n[judge]\ngate = \"on\"\nmodel = \"\"\ntimeout_ms = 30000\nmax_concurrent = 1\nmax_per_turn = 1\n\n[providers.openai]\nbase_url = \"http://127.0.0.1:{port}\"\napi = \"openai_chat\"\n",
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&config_text))?;
    let extension = history(HistoryConfig::Enabled { share: 0.4 })?;
    let letters = dal_ext::letter_extension(Arc::new(
        dal_ext::SkillRegistry::merge(&[]).expect("empty skill registry merges"),
    ))?;
    GrantStore::new(data.clone())
        .grant(
            GrantKey::for_extension(&extension, workspace.as_path())
                .expect("history has a code identity"),
            ClientId::new("history-judge"),
            Timestamp::now(),
        )
        .await?;
    let host = Host::start(
        Product {
            name: "dalgona",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![extension, letters],
            bundled: Vec::new(),
        },
        config,
        Env {
            vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
            cwd: workspace_dir,
            sandbox_helper: None,
        },
    )
    .await?;
    let agent = host
        .open(
            SessionRef::Resume {
                workspace: workspace.clone(),
                key: session.to_string().into(),
            },
            ClientId::new("history-judge"),
        )
        .await?;
    let mut subscription = agent.subscribe(None)?;
    let rounds_result = trigger_rounds(&agent, &mut subscription, rounds).await;
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    host.shutdown_after_quiet_wait(Duration::from_secs(10))
        .await;
    let notices = rounds_result?;

    let store = Store::new(data.clone(), workspace, dal_core::Product::Dalgona);
    let (mut journal, _) = store.open_session(session).await?;
    let dreams = dream_records(journal.records());
    journal.close().await?;
    let bodies = captured
        .bodies
        .lock()
        .expect("bodies lock is not poisoned")
        .clone();
    shutdown_judge_loopback(&mut servers).await?;
    let _ = fs::remove_dir_all(root).await;
    Ok(Observed {
        notices,
        bodies,
        dreams,
    })
}

/// The request bodies that carry the judge shared envelope.
fn judge_requests(bodies: &[String]) -> Vec<&str> {
    bodies
        .iter()
        .map(String::as_str)
        .filter(|body| body.contains("<<<SHARED"))
        .collect()
}

/// The user-message text of the one request carrying the judge envelope.
fn judge_submission(bodies: &[String]) -> String {
    let requests = judge_requests(bodies);
    assert_eq!(requests.len(), 1, "exactly one typed judge call is made");
    let request: sonic_rs::Value =
        sonic_rs::from_str(requests[0]).expect("provider request is JSON");
    let messages = request
        .get("messages")
        .and_then(|messages| messages.as_array())
        .expect("chat request carries messages");
    let mut submitted = String::new();
    for message in messages {
        let is_user = message.get("role").and_then(JsonValueTrait::as_str) == Some("user");
        if !is_user {
            continue;
        }
        let parts = message
            .get("content")
            .and_then(|content| content.as_array())
            .expect("user message carries content parts");
        for part in parts {
            if let Some(text) = part.get("text").and_then(JsonValueTrait::as_str) {
                submitted.push_str(text);
            }
        }
    }
    submitted
}

/// The shared context between the judge envelope markers.
fn shared_section(submitted: &str) -> &str {
    let open = "<<<SHARED\n";
    let start = submitted.find(open).expect("shared envelope opens") + open.len();
    let end = submitted
        .find("\nSHARED>>>")
        .expect("shared envelope closes");
    &submitted[start..end]
}

/// Asserts each of letters `1..=count` is labeled by id and followed by its
/// exact stored text, oldest first (`label_i < text_i < label_{i+1}`).
fn assert_oldest_first_texts(submitted: &str, count: usize) {
    let mut cursor = 0usize;
    for index in 1..=count {
        let label = format!("letter://history/1.{index}\n");
        let text = letter_text_body(index);
        let label_at = submitted[cursor..]
            .find(&label)
            .expect("each consolidated letter is labeled by id");
        let text_at = submitted[cursor + label_at..]
            .find(&text)
            .expect("each consolidated letter contributes its exact stored text");
        cursor += label_at + text_at + text.len();
    }
}

fn assert_parks_on_third_identical_failure(observed: &Observed) {
    assert_eq!(observed.notices.len(), 3, "three trigger rounds ran");
    assert!(
        observed.notices[0].starts_with(FAILED_PREFIX)
            && observed.notices[1].starts_with(FAILED_PREFIX),
        "the first two non-retryable failures notify without parking: {:?}",
        observed.notices
    );
    assert!(
        observed.notices[2].starts_with(PARKED_PREFIX),
        "the third identical failure parks; six transient failures would not: {:?}",
        observed.notices
    );
    assert!(
        judge_requests(&observed.bodies).is_empty(),
        "no judge call is made for a context that cannot be submitted"
    );
    assert!(
        observed.dreams.is_empty(),
        "no letter is consumed after a failure"
    );
}

#[tokio::test]
async fn history_consolidation_uses_session_judge_identity() -> TestResult {
    let observed = run_scenario(&Seed::uniform(), 1).await?;
    assert_eq!(
        observed.notices,
        [format!("Auto-dream consolidated {LETTERS} letters.")]
    );
    assert_eq!(
        observed.dreams.len(),
        1,
        "one history dream record is committed"
    );
    assert_eq!(observed.dreams[0].id, "dream/1");
    assert_eq!(
        observed.dreams[0].letters,
        (1..=LETTERS)
            .map(|index| format!("history/1.{index}"))
            .collect::<Vec<_>>(),
        "the dream consumes each seeded letter exactly once"
    );
    assert_eq!(observed.dreams[0].summary, DREAM_SUMMARY);
    let submitted = judge_submission(&observed.bodies);
    assert!(
        submitted.contains("Summary request:"),
        "the submitted context carries the dream summary request"
    );
    assert_oldest_first_texts(&submitted, LETTERS);
    Ok(())
}

#[tokio::test]
async fn history_consolidation_consumes_only_the_fitted_oldest_prefix() -> TestResult {
    let seed = Seed {
        texts: (1..=LETTERS)
            .map(|index| format!("{}: {}", letter_text_body(index), "p".repeat(PADDING)))
            .collect(),
        dangling: None,
    };
    let observed = run_scenario(&seed, 1).await?;
    assert_eq!(observed.dreams.len(), 1, "one dream record is committed");
    let fitted = observed.dreams[0].letters.len();
    assert!(
        (1..LETTERS).contains(&fitted),
        "a strict nonempty prefix fits the shared budget, got {fitted}"
    );
    assert_eq!(
        observed.dreams[0].letters,
        (1..=fitted)
            .map(|index| format!("history/1.{index}"))
            .collect::<Vec<_>>(),
        "the consumed ids are exactly the oldest fitted letters"
    );
    assert_eq!(
        observed.notices,
        [format!("Auto-dream consolidated {fitted} letters.")]
    );
    let submitted = judge_submission(&observed.bodies);
    assert!(
        shared_section(&submitted).len() <= SHARED_MAX,
        "the submitted shared context stays within the judge budget"
    );
    assert_oldest_first_texts(&submitted, fitted);
    assert!(
        !submitted.contains(&format!("letter://history/1.{}\n", fitted + 1)),
        "the first letter past the budget is left out of the call"
    );
    Ok(())
}

#[tokio::test]
async fn history_consolidation_parks_an_oldest_letter_that_cannot_fit() -> TestResult {
    let mut seed = Seed::uniform();
    seed.texts[0] = "x".repeat(OVERSIZED);
    let observed = run_scenario(&seed, 3).await?;
    assert_parks_on_third_identical_failure(&observed);
    Ok(())
}

#[tokio::test]
async fn history_consolidation_leaves_every_letter_when_one_text_cannot_resolve() -> TestResult {
    let mut seed = Seed::uniform();
    seed.dangling = Some(5);
    let observed = run_scenario(&seed, 3).await?;
    assert_parks_on_third_identical_failure(&observed);
    Ok(())
}
