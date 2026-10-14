// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! History battery letters reopen with their core compaction entry.
//!
//! A real `Host` runs a directly assembled product with the `history`
//! battery enabled over a scripted provider. The manual `/compact` path goes
//! through the production seam under test: the history compactor returns
//! image replacement parts plus fresh letter records, the fold emits the
//! compaction entry and one extension record per letter in one batch, and the
//! store stages blobs before journal bytes under one journal receipt. The
//! proof shuts the host down, reopens the store, and asserts that no durable
//! image part lacks a resolvable durable letter, that the entry precedes its
//! letters contiguously, and that a blob-publication failure, a journal write
//! failure inside the first letter, and a journal sync failure each commit
//! neither entry nor letters.
//!
//! The blob-publication failure stops before the journal append boundary. The
//! write and sync failures arm one real fault on the live session's journal
//! through the `dal-store` `test-support` feature, enabled here as a
//! dev-dependency feature, after the compaction batch is produced and before
//! its single journal append runs. Both roll back cleanly. A failed rollback
//! after a partial write is not covered: `dal-store` keeps the complete
//! leading records of such a batch on reopen, which only a persistent batch
//! boundary can change.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::manual_assert, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::time::Duration;

use dal_agent::{Env, Host, SessionRef};
use dal_core::{
    Answer, BlobId, ClientId, Command, Config, ConfigProduct, Entry, EntryKind, Expect,
    JournalPart, Part, Product as JournalProduct, Question, Record, SessionId, Stop, UpdateKind,
    Workspace,
};
use dal_store::Store;
use dalgona_batteries::history::{HistoryConfig, history};

fn events_step(input_tokens: u64) -> String {
    let output_tokens = dal_core::estimate_text_tokens(REPLY_TEXT);
    format!(
        "{{\"kind\":\"events\",\"events\":[{{\"type\":\"text_delta\",\"text\":\"{REPLY_TEXT}\"}},{{\"type\":\"tool_calls_done\",\"calls\":[]}},{{\"type\":\"usage\",\"usage\":{{\"input_tokens\":{input_tokens},\"cached_input_tokens\":0,\"output_tokens\":{output_tokens},\"reasoning_tokens\":null,\"cache_write_tokens\":0,\"cost_usd\":null}}}},{{\"type\":\"stop\",\"reason\":\"end_turn\"}}]}}\n"
    )
}

const COVERED_TEXT: &str = "old prefix ";
const RETAINED_TEXT: &str = "retained";
const REPLY_TEXT: &str = "recorded";

/// A unique temporary root built without environment reads: the workspace
/// crate owns `uuid`, and `temp_dir` stays outside the banned method set.
fn temp_root(tag: &str) -> PathBuf {
    let id = uuid::Uuid::new_v4().as_simple().to_string();
    std::env::temp_dir().join(format!("dalgona-history-{tag}-{id}"))
}

struct Fixture {
    _root: PathBuf,
    data: PathBuf,
    workspace: Workspace,
    host: Host,
    agent: dal_agent::Agent,
    session: SessionId,
}
async fn start_host(
    data: &Path,
    workspace: &Workspace,
    session: SessionRef,
) -> (Host, dal_agent::Agent) {
    let script = data.join("script.jsonl");
    let config_text = format!(
        "approval = \"all\"\nmodel = \"openai/gpt-6-luna\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        script.to_string_lossy().replace('\\', "\\\\")
    );
    let config =
        Config::load(ConfigProduct::Dalgona, data, "", Some(&config_text)).expect("config");
    let extension = history(HistoryConfig::Enabled { share: 0.4 }).expect("history battery");
    let product = dal_agent::Product {
        name: "dalgona",
        data_root: data.to_owned(),
        defaults: "",
        extensions: vec![extension],
        bundled: Vec::new(),
    };
    let env = Env {
        vars: BTreeMap::from([(OsString::from("OPENAI_API_KEY"), OsString::from("sk-test"))]),
        cwd: workspace.as_path().to_owned(),
        sandbox_helper: None,
    };
    let host = Host::start(product, config, env).await.expect("host");
    let agent = host
        .open(session, ClientId::new("history-compaction"))
        .await
        .expect("open history session");
    (host, agent)
}

/// Builds a fresh host over an existing durable session for a retry.
async fn retry_fixture(data: PathBuf, workspace: Workspace, session: SessionId) -> Fixture {
    let (host, agent) = start_host(
        &data,
        &workspace,
        SessionRef::Resume {
            workspace: workspace.clone(),
            key: session.to_string().into(),
        },
    )
    .await;
    Fixture {
        _root: data.parent().expect("data has a root").to_owned(),
        data,
        workspace,
        host,
        agent,
        session,
    }
}

/// Builds a real host over a directly assembled product with the `history`
/// battery enabled and the provider scripted. The product carries no other
/// compactor, so the history battery alone decides the manual compact.
async fn fixture(tag: &str) -> Fixture {
    let root = temp_root(tag);
    let data = root.join("data");
    let workspace_dir = root.join("workspace");
    tokio::fs::create_dir_all(&data)
        .await
        .expect("data directory");
    tokio::fs::create_dir_all(&workspace_dir)
        .await
        .expect("workspace directory");
    let workspace = Workspace::new(workspace_dir.clone()).expect("workspace");
    let prompts = [
        vec![COVERED_TEXT.repeat(1_400)],
        vec![COVERED_TEXT.repeat(1_400)],
        vec![RETAINED_TEXT.repeat(2_000); 10],
    ];
    let mut input_tokens = 0;
    let mut steps = String::new();
    for parts in &prompts {
        input_tokens += parts
            .iter()
            .map(|text| dal_core::estimate_text_tokens(text))
            .sum::<u64>();
        steps.push_str(&events_step(input_tokens));
        input_tokens += dal_core::estimate_text_tokens(REPLY_TEXT);
    }
    tokio::fs::write(data.join("script.jsonl"), steps)
        .await
        .expect("write provider script");
    let (host, agent) = start_host(
        &data,
        &workspace,
        SessionRef::New {
            workspace: workspace.clone(),
            name: None,
        },
    )
    .await;
    let mut subscription = agent.subscribe(None).expect("seed subscription");
    for parts in prompts {
        seed_turn(&agent, &mut subscription, parts).await;
    }
    let session = agent.session();
    let view = agent.view(dal_core::PageReq::default()).expect("seed view");
    for entry in view.entries.items {
        if let EntryKind::User { parts } = entry.kind {
            assert!(
                parts
                    .iter()
                    .all(|part| matches!(part, JournalPart::Text { .. })),
                "all source text must stay inline so the fault targets PNG publication"
            );
        }
    }
    Fixture {
        _root: root,
        data,
        workspace,
        host,
        agent,
        session,
    }
}

async fn seed_turn(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
    parts: Vec<String>,
) {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: parts
                .into_iter()
                .map(|text| Part::Text { text: text.into() })
                .collect(),
        })
        .await
        .expect("submit seed prompt");
    tokio::time::timeout(Duration::from_secs(10), async {
        while let Some(delivery) = subscription.next().await {
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            match &update.kind {
                UpdateKind::RequestOpened(request) => {
                    assert!(
                        matches!(request.question, Question::Grant { .. }),
                        "unexpected history request"
                    );
                    agent
                        .answer(request.id, Answer::ApproveForSession)
                        .await
                        .expect("authorize history services");
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn, "seed provider turn must succeed");
                    return;
                }
                UpdateKind::Notice(notice) => {
                    assert!(
                        !notice.kind.starts_with("hook."),
                        "history hook failed: {}",
                        notice.text
                    );
                }
                _ => {}
            }
        }
        panic!("seed subscription closed before the turn ended");
    })
    .await
    .expect("seed turn ends");
}

/// Whether one notice kind ends a manual compaction: the committed notice or
/// the refusal notice.
fn terminal_compaction_notice(kind: &str) -> bool {
    matches!(
        kind,
        "compaction_ended"
            | "compact.manual_failed"
            | "compaction_none"
            | "compact.nothing"
            | "compact.already"
    )
}

/// The terminal notice of one manual compaction: the committed kind and
/// text, or the refusal kind and text. Matching both keeps a refusal from
/// hanging the waiter past its timeout. A timeout or a closed subscription
/// names every notice seen and the delivery count first, so a silent settle
/// is diagnosable from the failure alone.
async fn compaction_end(subscription: &mut dal_agent::Subscription) -> (Box<str>, Box<str>) {
    let mut seen: Vec<(Box<str>, Box<str>)> = Vec::new();
    let mut deliveries = 0_usize;
    let wait = tokio::time::timeout(Duration::from_secs(60), async {
        while let Some(delivery) = subscription.next().await {
            deliveries += 1;
            let dal_agent::Delivery::Update(update) = delivery else {
                continue;
            };
            let dal_core::UpdateKind::Notice(notice) = &update.kind else {
                continue;
            };
            if terminal_compaction_notice(notice.kind.as_ref()) {
                return Some((notice.kind.clone(), notice.text.clone()));
            }
            seen.push((notice.kind.clone(), notice.text.clone()));
        }
        None
    })
    .await;
    match wait {
        Ok(Some(end)) => end,
        Ok(None) => panic!(
            "the subscription stays open until the compaction ends: {deliveries} deliveries, notices {seen:?}"
        ),
        Err(error) => {
            panic!("compaction ends in time ({error:?}): {deliveries} deliveries, notices {seen:?}")
        }
    }
}

/// One durable letter in journal order.
struct Letter {
    id: String,
    png_blob: String,
    png_bytes: u32,
    spans: Vec<(u64, u32, u32, u32)>,
    letters: Vec<u32>,
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct LetterBody {
    kind: String,
    v: u8,
    id: String,
    png_blob: String,
    png_bytes: u32,
    width: u32,
    height: u32,
    cell: [u8; 2],
    spans: Vec<(u64, u32, u32, u32)>,
    letters: Vec<u32>,
}

impl LetterBody {
    /// Checks the exact persisted compaction schema without trusting the producer.
    fn checked(self) -> Option<Letter> {
        if self.kind != "compaction" || self.v != 1 {
            return None;
        }
        let lowercase_digest = self.png_blob.len() == 64
            && self
                .png_blob
                .bytes()
                .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
        if !lowercase_digest {
            return None;
        }
        let sized = self.width > 0 && self.height > 0 && self.png_bytes > 0;
        let bounded_cell = self.cell.iter().all(|side| (1..=64).contains(side));
        let valid_spans = !self.spans.is_empty()
            && self
                .spans
                .iter()
                .all(|(entry, _, _, len)| *entry != 0 && *len != 0);
        if !(sized && bounded_cell && valid_spans && !self.id.is_empty()) {
            return None;
        }
        Some(Letter {
            id: self.id,
            png_blob: self.png_blob,
            png_bytes: self.png_bytes,
            spans: self.spans,
            letters: self.letters,
        })
    }
}

/// Collects the durable compaction entry and its letters in journal order.
///
/// Returns `None` while the reopened session has no compaction entry. The
/// letters must follow the entry contiguously: the fold emits them in one
/// batch, and the store journalizes one batch under one receipt.
fn compaction_letters(records: &[Record]) -> Option<(&Entry, Vec<Letter>)> {
    let entry_index = records.iter().position(|record| {
        matches!(
            record,
            Record::Compaction(entry)
                if matches!(&entry.kind, EntryKind::Compaction { .. })
        )
    })?;
    let Record::Compaction(entry) = &records[entry_index] else {
        return None;
    };
    let mut letters = Vec::new();
    for record in records.iter().skip(entry_index + 1) {
        let Record::Ext {
            ext, kind, body, ..
        } = record
        else {
            break;
        };
        if ext.as_ref() != "history" || kind.as_ref() != "letter" {
            break;
        }
        let decoded: LetterBody = sonic_rs::from_str(body.as_str()).expect("letter decodes");
        let letter = decoded.checked().expect("letter is structurally valid");
        if letter.letters.contains(&0) {
            panic!(
                "letter {} carries a non-one-based source-letter index",
                letter.id
            );
        }
        let ordinal = letter.id.rsplit('/').next().expect("letter has an ordinal");
        let dot = ordinal.find('.').expect("letter id has ordinal.index form");
        let (before, after) = ordinal.split_at(dot);
        if before.parse::<u32>().is_err() || after[1..].parse::<u32>().is_err() {
            panic!(
                "letter id {:?} must be history/<ordinal>.<index>",
                letter.id
            );
        }
        letters.push(letter);
    }
    Some((entry, letters))
}

/// Resolves one entry image part to its durable bytes: inline base64, or a
/// blob digest read back through the reopened journal.
fn image_bytes(
    journal: &dal_store::Journal,
    mime: &str,
    encoded: &str,
    declared_blob_bytes: Option<u64>,
    index: usize,
) -> Vec<u8> {
    use base64::Engine as _;
    if mime != "image/png" {
        panic!("image part {index} must be image/png, not {mime}");
    }
    if declared_blob_bytes.is_some() {
        let id = BlobId::parse(encoded).expect("blob digest parses");
        return journal.read_blob(id).expect("blob bytes are durable");
    }
    base64::engine::general_purpose::STANDARD
        .decode(encoded)
        .expect("entry image decodes")
}

/// Asserts every image part resolves to a durable letter with the same PNG
/// digest and every declared byte length matches the reopened bytes.
fn assert_resolvable(journal: &dal_store::Journal, entry: &Entry, letters: &[Letter]) {
    let EntryKind::Compaction { parts, .. } = &entry.kind else {
        panic!("the reopened entry must stay a compaction entry");
    };
    let images: Vec<(&str, &str, Option<u64>)> = parts
        .iter()
        .filter_map(|part| match part {
            JournalPart::Image { mime, base64 } => Some((mime.as_ref(), base64.as_ref(), None)),
            JournalPart::ImageBlob { mime, blob, bytes } => {
                Some((mime.as_ref(), blob.as_ref(), Some(*bytes)))
            }
            _ => None,
        })
        .collect();
    if images.is_empty() {
        panic!("the history compactor must commit image parts for this fixture");
    }
    if images.len() != letters.len() {
        panic!(
            "one letter per drawn image: {} images but {} letters",
            images.len(),
            letters.len()
        );
    }
    for (index, ((mime, encoded, declared_blob_bytes), letter)) in
        images.iter().zip(letters.iter()).enumerate()
    {
        let bytes = image_bytes(journal, mime, encoded, *declared_blob_bytes, index);
        let actual_bytes = u64::try_from(bytes.len()).expect("PNG length fits u64");
        if u64::from(letter.png_bytes) != actual_bytes {
            panic!(
                "letter {} declares {} PNG bytes but reopened image has {actual_bytes}",
                letter.id, letter.png_bytes
            );
        }
        if let Some(declared) = declared_blob_bytes
            && *declared != actual_bytes
        {
            panic!(
                "image part {index} declares {declared} blob bytes but reopened image has {actual_bytes}"
            );
        }
        let expected =
            blake3::Hash::from_hex(letter.png_blob.as_str()).expect("letter digest parses");
        if expected.as_bytes() != blake3::hash(&bytes).as_bytes() {
            panic!(
                "image part {index} must resolve to letter {} digest {}",
                letter.id, letter.png_blob
            );
        }
        for &(_, _, _, len) in &letter.spans {
            if len == 0 {
                panic!("letter {} carries an empty span", letter.id);
            }
        }
    }
}

/// Opens one durable journal copy after the host released its session lock.
async fn reopen(data: &Path, workspace: &Workspace, session: SessionId) -> dal_store::Journal {
    let store = Store::new(data.to_owned(), workspace.clone(), JournalProduct::Dalgona);
    let (journal, _) = store.open_session(session).await.expect("reopen session");
    journal
}
fn compaction_count(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| matches!(record, Record::Compaction(_)))
        .count()
}

fn history_letter_count(records: &[Record]) -> usize {
    records
        .iter()
        .filter(|record| {
            matches!(
                record,
                Record::Ext { ext, kind, .. }
                    if ext.as_ref() == "history" && kind.as_ref() == "letter"
            )
        })
        .count()
}

/// Lists durable blob files so this pre-journal failure can account for
/// publication effects without claiming to cover a later append failure.
fn blob_files(path: &Path) -> Vec<OsString> {
    let mut files = std::fs::read_dir(path)
        .expect("blob directory")
        .filter_map(|entry| {
            let entry = entry.expect("blob directory entry");
            entry
                .file_type()
                .expect("blob entry type")
                .is_file()
                .then_some(entry.file_name())
        })
        .collect::<Vec<_>>();
    files.sort_unstable();
    files
}

/// Shuts the fixture host down so the reopened store owns the session lock.
async fn shutdown(fixture: Fixture) -> (PathBuf, Workspace, SessionId) {
    let Fixture {
        data,
        workspace,
        host,
        agent,
        session,
        ..
    } = fixture;
    drop(agent);
    host.close(session).await.expect("close session");
    host.shutdown_after_quiet_wait(Duration::from_secs(10))
        .await;
    (data, workspace, session)
}

/// The reopened compaction entry is followed by its durable letters.
///
/// The reopened journal shows the compaction entry first, then exactly the
/// history letter records contiguously, and every image part resolves to a
/// durable letter digest. This happy-path check does not exercise an append
/// failure after the batch reaches the journal.
#[tokio::test]
async fn history_compaction_reopens_letters_with_the_entry() {
    let fixture = fixture("commit").await;
    let mut subscription = fixture.agent.subscribe(None).expect("subscribe");
    fixture
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");

    let (kind, text) = compaction_end(&mut subscription).await;
    if kind.as_ref() != "compaction_ended" || !text.starts_with("Context compacted by history:") {
        panic!("the history battery must commit before any fallback: {kind} {text}");
    }
    let (data, workspace, session) = shutdown(fixture).await;
    let mut journal = reopen(&data, &workspace, session).await;
    let records = journal.records().to_vec();
    let (entry, letters) = compaction_letters(&records).expect("reopened compaction entry");
    if letters.is_empty() {
        panic!("the committed compaction must carry durable letters");
    }
    assert_resolvable(&journal, entry, &letters);
    journal.close().await.expect("close reopened journal");
}

/// A supported blob-publication refusal is inspected immediately after reopen.
///
/// Swapping the session blob directory for a regular file revokes blob
/// publication before the compaction batch reaches journal append. The failed
/// host is closed, its complete durable record list is reopened and checked
/// for zero compactions and zero history letters, and the blob inventory is
/// compared before a fresh host retries successfully. This is not a journal
/// append-fault proof; that remaining obligation is documented above.
#[tokio::test]
async fn failed_blob_publication_reopens_cleanly_before_retry() {
    let fixture = fixture("failure").await;
    let mut subscription = fixture.agent.subscribe(None).expect("subscribe");

    let probe = Store::new(
        fixture.data.clone(),
        fixture.workspace.clone(),
        JournalProduct::Dalgona,
    );
    let session_dir = probe
        .session_file(fixture.session)
        .parent()
        .expect("session file has a directory")
        .to_owned();
    drop(probe);
    let blobs = session_dir.join("blobs");
    let backup = session_dir.join("blobs-backup");
    let had_dir = blobs.is_dir();
    let before_blobs = if had_dir {
        blob_files(&blobs)
    } else {
        Vec::new()
    };
    if had_dir {
        std::fs::rename(&blobs, &backup).expect("park blob directory");
    }
    std::fs::write(&blobs, b"blocked").expect("block blob publication");
    fixture
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    let (kind, text) = compaction_end(&mut subscription).await;
    if kind.as_ref() != "compact.manual_failed" || !text.starts_with("Compaction failed:") {
        panic!("blocked publication must refuse the compaction: {kind} {text}");
    }

    let (data, workspace, session) = shutdown(fixture).await;
    let mut failed_journal = reopen(&data, &workspace, session).await;
    let failed_records = failed_journal.records().to_vec();
    let failed_compactions = compaction_count(&failed_records);
    if failed_compactions != 0 {
        panic!("the refused pre-journal attempt must leave zero compactions: {failed_compactions}");
    }
    let failed_letters = history_letter_count(&failed_records);
    if failed_letters != 0 {
        panic!("the refused pre-journal attempt must leave zero history letters: {failed_letters}");
    }
    failed_journal
        .close()
        .await
        .expect("close immediate failed-attempt reopen");

    std::fs::remove_file(&blobs).expect("unblock blob publication");
    if had_dir {
        std::fs::rename(&backup, &blobs).expect("restore blob directory");
    } else {
        std::fs::create_dir_all(&blobs).expect("recreate blob directory");
    }
    let after_blobs = blob_files(&blobs);
    if before_blobs != after_blobs {
        panic!(
            "the refused pre-journal attempt changed durable blobs: before {before_blobs:?}, after {after_blobs:?}"
        );
    }

    let retry = retry_fixture(data, workspace, session).await;
    let mut retry_subscription = retry.agent.subscribe(None).expect("retry subscribe");
    retry
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("retry compaction after restore");
    let (kind, text) = compaction_end(&mut retry_subscription).await;
    if kind.as_ref() != "compaction_ended" || !text.starts_with("Context compacted by history:") {
        panic!("restored publication must commit: {kind} {text}");
    }
    let (data, workspace, session) = shutdown(retry).await;
    let mut journal = reopen(&data, &workspace, session).await;
    let records = journal.records().to_vec();
    let compactions = compaction_count(&records);
    if compactions != 1 {
        panic!("the retried compaction must leave exactly one entry: {compactions}");
    }
    let (entry, letters) = compaction_letters(&records).expect("reopened compaction entry");
    if letters.is_empty() {
        panic!("the retried compaction must carry durable letters");
    }
    assert_resolvable(&journal, entry, &letters);
    journal.close().await.expect("close reopened journal");
}

/// The encoded lengths of the compaction entry line and of the whole
/// compaction batch (entry plus its contiguous history letters), measured
/// from a committed run of the same fixture so a write fault can be aimed
/// inside the first letter of the next batch.
async fn committed_batch_lengths() -> (usize, usize) {
    let fixture = fixture("measure").await;
    let mut subscription = fixture.agent.subscribe(None).expect("subscribe");
    fixture
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    let (kind, text) = compaction_end(&mut subscription).await;
    if kind.as_ref() != "compaction_ended" || !text.starts_with("Context compacted by history:") {
        panic!("the measuring run must commit: {kind} {text}");
    }
    let (data, workspace, session) = shutdown(fixture).await;
    let mut journal = reopen(&data, &workspace, session).await;
    let records = journal.records().to_vec();
    let entry_index = records
        .iter()
        .position(|record| matches!(record, Record::Compaction(_)))
        .expect("measured compaction entry");
    let entry_len = dal_core::encode(&records[entry_index])
        .expect("entry encodes")
        .len();
    let mut batch_len = entry_len;
    for record in &records[entry_index + 1..] {
        let Record::Ext { ext, kind, .. } = record else {
            break;
        };
        if ext.as_ref() != "history" || kind.as_ref() != "letter" {
            break;
        }
        batch_len += dal_core::encode(record).expect("letter encodes").len();
    }
    journal.close().await.expect("close measured journal");
    (entry_len, batch_len)
}

/// The real journal fault armed on the live session before the compaction
/// batch reaches the store append.
enum AppendFault {
    /// The batch write fails inside the first letter record, after the whole
    /// compaction entry line already landed in the file.
    WriteInsideFirstLetter,
    /// Every batch byte lands, then the durability sync fails.
    SyncAfterFullWrite,
}

/// Waits until the session actor refuses further writes. A failed journal
/// append publishes no notice (updates follow the receipt), so the refusal
/// is the observable end of the failed compaction. Outside `Idle`,
/// `Compact` answers `BusyTurn`; an accepted probe means the fault never
/// failed the batch.
async fn await_session_broken(agent: &dal_agent::Agent) {
    let wait = tokio::time::timeout(Duration::from_secs(60), async {
        loop {
            match agent.submit(Command::Compact { focus: None }).await {
                Err(error) if error.to_string().contains("journal write failed") => return,
                Err(_) => tokio::time::sleep(Duration::from_millis(25)).await,
                Ok(_) => panic!("the armed append fault never failed the compaction batch"),
            }
        }
    })
    .await;
    wait.expect("the failed compaction batch refuses writes in time");
}

/// Arms one real fault on the live session's journal, runs the manual
/// compaction through the production seam, and proves the failed batch left
/// no trace: the journal is byte-identical to its pre-compaction state, a
/// reopen shows zero compaction entries and zero history letters, and a fresh
/// host then commits the compaction normally.
async fn failed_append_leaves_no_entry_or_letter(tag: &str, fault: AppendFault) {
    let write_cut = match fault {
        AppendFault::WriteInsideFirstLetter => {
            let (entry_len, batch_len) = committed_batch_lengths().await;
            // The cut lands past the whole entry line (timestamps vary the
            // line by a few bytes) and strictly inside the first letter.
            let cut = entry_len + 16;
            assert!(
                cut < batch_len,
                "the cut {cut} must land inside the {batch_len}-byte batch"
            );
            Some(cut)
        }
        AppendFault::SyncAfterFullWrite => None,
    };
    let fixture = fixture(tag).await;
    let store = Store::new(
        fixture.data.clone(),
        fixture.workspace.clone(),
        JournalProduct::Dalgona,
    );
    let journal_path = store.session_file(fixture.session);
    let before = std::fs::read(&journal_path).expect("read the seeded journal");
    match write_cut {
        Some(cut) => store.arm_append_write_fault(fixture.session, cut),
        None => store.arm_append_sync_fault(fixture.session),
    }
    fixture
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("manual compaction is accepted");
    await_session_broken(&fixture.agent).await;
    let (data, workspace, session) = shutdown(fixture).await;

    let after = std::fs::read(&journal_path).expect("read the journal after the failed batch");
    if after != before {
        panic!(
            "the failed batch must leave the journal byte-identical: {} bytes before, {} after",
            before.len(),
            after.len()
        );
    }
    let mut failed_journal = reopen(&data, &workspace, session).await;
    let failed_records = failed_journal.records().to_vec();
    let failed_compactions = compaction_count(&failed_records);
    if failed_compactions != 0 {
        panic!("the failed batch must leave zero compactions: {failed_compactions}");
    }
    let failed_letters = history_letter_count(&failed_records);
    if failed_letters != 0 {
        panic!("the failed batch must leave zero history letters: {failed_letters}");
    }
    failed_journal
        .close()
        .await
        .expect("close the reopened failed journal");

    let retry = retry_fixture(data, workspace, session).await;
    let mut retry_subscription = retry.agent.subscribe(None).expect("retry subscribe");
    retry
        .agent
        .submit(Command::Compact { focus: None })
        .await
        .expect("retry compaction is accepted");
    let (kind, text) = compaction_end(&mut retry_subscription).await;
    if kind.as_ref() != "compaction_ended" || !text.starts_with("Context compacted by history:") {
        panic!("the retry after the failed batch must commit: {kind} {text}");
    }
    let (data, workspace, session) = shutdown(retry).await;
    let mut journal = reopen(&data, &workspace, session).await;
    let records = journal.records().to_vec();
    let compactions = compaction_count(&records);
    if compactions != 1 {
        panic!("the retried compaction must leave exactly one entry: {compactions}");
    }
    let (entry, letters) = compaction_letters(&records).expect("reopened compaction entry");
    if letters.is_empty() {
        panic!("the retried compaction must carry durable letters");
    }
    assert_resolvable(&journal, entry, &letters);
    journal.close().await.expect("close reopened journal");
}

/// A journal write failure inside the first letter, after the whole
/// compaction entry line landed, rolls the single batch back: neither the
/// entry nor any letter is durable after reopen.
#[tokio::test]
async fn journal_write_fault_inside_a_letter_commits_nothing() {
    failed_append_leaves_no_entry_or_letter("write-fault", AppendFault::WriteInsideFirstLetter)
        .await;
}

/// A journal sync failure after every batch byte landed rolls the whole
/// batch back: neither the entry nor any letter is durable after reopen.
#[tokio::test]
async fn journal_sync_fault_after_full_write_commits_nothing() {
    failed_append_leaves_no_entry_or_letter("sync-fault", AppendFault::SyncAfterFullWrite).await;
}
