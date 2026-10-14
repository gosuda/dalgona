// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The history task opens its judge from the session setup identity.
//!
//! A real resumed `Host` is seeded with thirty durable history letters, then
//! runs one ordinary scripted turn. The settled hook reaches auto-dream;
//! success requires the task's setup-captured `[judge]` configuration and
//! session model route to make the summary request, rather than silently
//! taking the gate-off path from an empty `JudgeOpen` identity.
#![expect(clippy::expect_used, reason = "test assertions abort on failure")]
#![expect(clippy::panic, reason = "test assertions abort on failure")]

use std::collections::BTreeMap;
use std::error::Error as StdError;
use std::ffi::OsString;
use std::io::Cursor;
use std::num::NonZeroU64;
use std::path::Path;
use std::time::Duration;

use dal_agent::ext::grants::{GrantKey, GrantStore};
use dal_agent::{Delivery, Env, Host, Product, SessionRef};
use dal_core::{
    ClientId, Command, Config, ConfigProduct, Entry, EntryId, EntryKind, Expect, JournalPart, Name,
    Origin, Part, RawJson, Record, SessionId, Stop, Timestamp, UpdateKind, Workspace,
};
use dal_store::Store;
use dalgona_batteries::history::{HistoryConfig, history};
use serde::Deserialize;
use sonic_rs::JsonValueTrait;
use tokio::fs;

const MODEL: &str = "openai/gpt-6-luna";
const LETTERS: usize = 30;
const DREAM_NOTICE: &str = "Auto-dream consolidated 30 letters.";
const DREAM_SUMMARY: &str = "the captured history summary";
const WAIT: Duration = Duration::from_secs(30);
const USAGE: &str = r#"{"type":"usage","usage":{"input_tokens":10,"cached_input_tokens":0,"output_tokens":5,"reasoning_tokens":null,"cache_write_tokens":0,"cost_usd":null}}"#;

type TestResult = Result<(), Box<dyn StdError>>;

#[derive(Deserialize)]
struct DreamBody {
    id: String,
    letters: Vec<String>,
    summary: String,
}

fn entry_id(value: u64) -> EntryId {
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

fn png_bytes() -> Vec<u8> {
    let mut bytes = Vec::new();
    {
        let cursor = Cursor::new(&mut bytes);
        let mut encoder = png::Encoder::new(cursor, 1, 1);
        encoder.set_color(png::ColorType::Grayscale);
        encoder.set_depth(png::BitDepth::Eight);
        let mut writer = encoder.write_header().expect("PNG header");
        writer.write_image_data(&[0]).expect("PNG pixels");
        writer.finish().expect("PNG finish");
    }
    bytes
}

fn letter_record(index: usize, png_blob: &str, png_bytes: u32) -> Record {
    let body = format!(
        r#"{{"v":1,"id":"skill/{index}","kind":"skill","png_blob":"{png_blob}","png_bytes":{png_bytes},"width":1,"height":1,"cell":[1,1],"spans":[]}}"#
    );
    Record::Ext {
        at: jiff::Timestamp::UNIX_EPOCH,
        ext: "history".into(),
        kind: "letter".into(),
        body: RawJson::parse(&body).expect("letter body parses"),
    }
}

async fn seed_session(data: &Path, workspace: &Workspace, session: SessionId) {
    let store = Store::new(
        data.to_owned(),
        workspace.clone(),
        dal_core::Product::Dalgona,
    );
    let mut journal = store.create_session(session);
    journal
        .append(vec![user_record()])
        .await
        .expect("seed user");
    let png = png_bytes();
    let png_len = u32::try_from(png.len()).expect("PNG length fits");
    let blob = journal.put_blob(png).expect("store PNG blob");
    let png_blob = blob.to_string();
    journal
        .append(
            (1..=LETTERS)
                .map(|index| letter_record(index, &png_blob, png_len))
                .collect(),
        )
        .await
        .expect("seed history letters");
    journal.close().await.expect("close seeded journal");
}

fn events_step(text: &str) -> String {
    format!(
        r#"{{"kind":"events","events":[{{"type":"text_delta","text":"{text}"}},{{"type":"tool_calls_done","calls":[]}},{USAGE},{{"type":"stop","reason":"end_turn"}}]}}"#
    )
}

async fn await_dream(
    agent: &dal_agent::Agent,
    subscription: &mut dal_agent::Subscription,
) -> TestResult {
    agent
        .submit(Command::Prompt {
            expect: Expect::Idle,
            content: vec![Part::Text {
                text: "trigger the settled history task".into(),
            }],
        })
        .await?;
    tokio::time::timeout(WAIT, async {
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
                UpdateKind::Notice(notice)
                    if notice.kind.as_ref() == "history"
                        && notice.text.as_ref() == DREAM_NOTICE =>
                {
                    return Ok::<(), Box<dyn StdError>>(());
                }
                UpdateKind::TurnEnded { stop, .. } => {
                    assert_eq!(*stop, Stop::EndTurn, "scripted trigger turn succeeds");
                }
                _ => {}
            }
        }
        Err("subscription closed before auto-dream committed".into())
    })
    .await
    .map_err(|_| "history judge regression timed out")??;
    Ok(())
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

#[tokio::test]
async fn history_consolidation_uses_session_judge_identity() -> TestResult {
    let root = std::env::temp_dir().join(format!("dalgona-history-judge-{}", uuid::Uuid::new_v4()));
    let data = root.join("data");
    let workspace_dir = root.join("workspace");
    fs::create_dir_all(&data).await?;
    fs::create_dir_all(&workspace_dir).await?;
    let workspace = Workspace::new(workspace_dir.clone())?;
    let session = SessionId::new_v7();
    seed_session(&data, &workspace, session).await;

    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        format!(
            "{}\n{}\n",
            events_step("turn complete"),
            events_step(DREAM_SUMMARY)
        ),
    )
    .await?;
    let config_text = format!(
        "approval = \"all\"\nmodel = \"{MODEL}\"\n\n[judge]\ngate = \"on\"\nmodel = \"\"\ntimeout_ms = 30000\nmax_concurrent = 1\nmax_per_turn = 1\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.to_string_lossy().replace('\\', "\\\\")
    );
    let config = Config::load(ConfigProduct::Dalgona, &data, "", Some(&config_text))?;
    let extension = history(HistoryConfig::Enabled { share: 0.4 })?;
    GrantStore::new(data.clone())
        .grant(
            GrantKey {
                extension: Name::parse("history")?,
                origin: Origin::Bundled,
                services: extension.inject().capabilities(),
                        scope: Some(dal_agent::GrantScope::for_workspace(
                            extension.source_digest().expect("bundled code digest"),
                            &workspace,
                        )),
            },
            ClientId::new("history-judge"),
            Timestamp::now(),
        )
        .await?;
    let host = Host::start(
        Product {
            name: "dalgona",
            data_root: data.clone(),
            defaults: "",
            extensions: vec![extension],
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
    let dream_result = await_dream(&agent, &mut subscription).await;
    drop(subscription);
    drop(agent);
    host.close(session).await?;
    host.shutdown_after_quiet_wait(Duration::from_secs(10))
        .await;
    dream_result?;

    let store = Store::new(data.clone(), workspace, dal_core::Product::Dalgona);
    let (mut journal, _) = store.open_session(session).await?;
    let dreams = dream_records(journal.records());
    assert_eq!(dreams.len(), 1, "one history dream record is committed");
    assert_eq!(dreams[0].id, "dream/1");
    assert_eq!(
        dreams[0].letters,
        (1..=LETTERS)
            .map(|index| format!("skill/{index}"))
            .collect::<Vec<_>>(),
        "the dream consumes each seeded letter exactly once"
    );
    assert_eq!(dreams[0].summary, DREAM_SUMMARY);
    journal.close().await?;
    let _ = fs::remove_dir_all(root).await;
    Ok(())
}
