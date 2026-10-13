// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! dalgona success-criterion gate: a run report is delivered exactly once
//! through the real host. One `agents` run whose child reports `done` is
//! delivered as one message that names the run, the journal records one wake
//! for that run, and no second delivery follows once the session is quiet.
use dalgona_gates::support;

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

const RUN_CALL: &str = r#"{"action":"run","steps":[{"name":"solo","prompt":"do the thing"}]}"#;

const REPORT_CALL: &str = r#"{"status":"done","report":"did the thing"}"#;

const DELIVERY_WAIT: Duration = Duration::from_secs(30);

const QUIET_WINDOW: Duration = Duration::from_secs(3);

struct TestRoot(PathBuf);

impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn run_report_is_delivered_exactly_once() -> support::TestResult<()> {
    let root =
        TestRoot(std::env::temp_dir().join(format!("dalgona-once-{}", uuid::Uuid::now_v7())));
    let data = root.0.join("data");
    let workspace = root.0.join("workspace");
    fs::create_dir_all(&data)?;
    fs::create_dir_all(&workspace)?;
    // Steps 1 and 2: the parent starts the run and ends its turn. Steps 3 and
    // 4: the child reports done, then answers the report result. Step 5: the
    // delivery wake turn ends.
    let fixture = data.join("script.jsonl");
    fs::write(
        &fixture,
        support::tool_step("r1", "agents", RUN_CALL)
            + "\n"
            + &support::text_step("started")
            + "\n"
            + &support::tool_step("c1", "orchestration__report", REPORT_CALL)
            + "\n"
            + &support::text_step("ack")
            + "\n"
            + &support::text_step("delivered"),
    )?;
    let user = format!(
        "model = \"openai/gpt-6-luna\"\n\n[judge]\ngate = \"off\"\n\n[providers.scripted]\nfixture = \"{}\"\n",
        fixture.display()
    );
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(deliver_the_run_report(&data, workspace, &user))
}

/// Grants every bundled extension the services it declares, as a person
/// would on first use, so no grant question interrupts the run.
async fn grant_bundled_extensions(data: &Path, user: &str) -> support::TestResult<()> {
    let product = support::build_product(data.to_path_buf(), Some(user))?;
    for extension in &product.extensions {
        let services = extension.inject().capabilities();
        if services.is_empty() {
            continue;
        }
        dal_agent::ext::grants::GrantStore::new(data.to_path_buf())
            .grant(
                dal_agent::ext::grants::GrantKey {
                    extension: dal_core::Name::parse(extension.name())?,
                    origin: dal_core::Origin::Bundled,
                    services,
                },
                dal_core::ClientId::new("once-gate"),
                dal_core::Timestamp::now(),
            )
            .await?;
    }
    Ok(())
}

/// Starts the run through the real host, waits out the delivery wake and a
/// quiet window, and checks what the journal holds.
async fn deliver_the_run_report(
    data: &Path,
    workspace: PathBuf,
    user: &str,
) -> support::TestResult<()> {
    grant_bundled_extensions(data, user).await?;
    let host = support::start_dalgona_with_config(data.to_path_buf(), Some(user)).await?;
    let agent = host
        .open(
            dal_agent::SessionRef::New {
                workspace: dal_core::Workspace::new(workspace)?,
                name: None,
            },
            dal_core::ClientId::new("once-gate"),
        )
        .await?;
    let session = agent.view(dal_core::PageReq::default())?.session.id;
    agent
        .submit(dal_core::Command::Prompt {
            expect: dal_core::Expect::Idle,
            content: vec![dal_core::Part::Text {
                text: "start the run".into(),
            }],
        })
        .await?;
    let journal_path = wait_for_wake_turn_end(data, &session.to_string()).await?;
    tokio::time::sleep(QUIET_WINDOW).await;
    assert_delivered_once(&fs::read_to_string(&journal_path)?)?;
    let report = host.shutdown(Duration::from_secs(5)).await;
    assert_eq!(report.tasks_remaining, 0);
    Ok(())
}

/// The run report reached the model once, one wake named the run, and no
/// turn ran after the delivery turn.
fn assert_delivered_once(journal: &str) -> support::TestResult<()> {
    let run = started_run_id(journal)
        .ok_or_else(|| format!("the agents result never named the run: {journal}"))?;
    let delivered: Vec<&str> = support::journal_records(journal, "user")
        .into_iter()
        .filter(|record| record.contains(run))
        .collect();
    assert_eq!(
        delivered.len(),
        1,
        "the run report must reach the model once: {delivered:?}\n{journal}"
    );
    assert!(
        delivered[0].contains("did the thing"),
        "the delivered message carries the child's report: {}",
        delivered[0]
    );
    let wakes: Vec<&str> = support::journal_records(journal, "wake_attempt")
        .into_iter()
        .filter(|record| record.contains(run))
        .collect();
    assert_eq!(
        wakes.len(),
        1,
        "exactly one wake may name the run: {wakes:?}\n{journal}"
    );
    assert_eq!(
        support::journal_records(journal, "turn_end").len(),
        2,
        "the prompt turn and one delivery turn, nothing after:\n{journal}"
    );
    Ok(())
}

/// The run id the `agents` start result printed: `started run <id> "<label>"`.
fn started_run_id(journal: &str) -> Option<&str> {
    journal
        .split("started run ")
        .nth(1)?
        .split([' ', '\\'])
        .next()
        .filter(|id| !id.is_empty())
}

/// Waits until the delivery wake has ended its turn: the prompt turn plus
/// the wake turn are both closed in the parent's journal. The journal appears
/// with the session's first write, so its path is resolved inside the wait.
async fn wait_for_wake_turn_end(data: &Path, session: &str) -> support::TestResult<PathBuf> {
    let mut last = String::new();
    let waited = tokio::time::timeout(DELIVERY_WAIT, async {
        loop {
            if let Some(path) = session_journal(data, session) {
                last = fs::read_to_string(&path).unwrap_or_default();
                if support::journal_records(&last, "turn_end").len() >= 2 {
                    return path;
                }
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    })
    .await;
    let path =
        waited.map_err(|_| format!("the run report was never delivered within 30 s:\n{last}"))?;
    Ok(path)
}

/// The journal of the parent session, found by its session id. A child
/// session keeps its own journal beside it.
fn session_journal(data: &Path, session: &str) -> Option<PathBuf> {
    let mut journals = Vec::new();
    collect(&data.join("sessions"), &mut journals).ok()?;
    journals.into_iter().find(|path| {
        path.parent()
            .and_then(Path::file_name)
            .is_some_and(|name| name == session)
    })
}

fn collect(dir: &Path, found: &mut Vec<PathBuf>) -> std::io::Result<()> {
    for entry in fs::read_dir(dir)? {
        let path = entry?.path();
        if path.is_dir() {
            collect(&path, found)?;
        } else if path.file_name().is_some_and(|name| name == "journal.jsonl") {
            found.push(path);
        }
    }
    Ok(())
}
