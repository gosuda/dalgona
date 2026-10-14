//! Helper process for lock-contention and kill tests.
//!
//! Usage: `append-child <data-root> <workspace-path> <session-id> <mode> [count]`.
//! Mode `hold` opens the session, appends one user entry, prints `ready` only
//! after its receipt, then sleeps with the journal (and its lock) held.
//! Mode `append` publishes `count` name records, printing `acked <n>` after
//! each receipt, then exits.
//! Mode `append-hold` appends one acknowledged multi-record user batch, prints
//! `acked-batch <count>`, then waits for an explicit `hold` command and keeps
//! waiting for release.

use std::{
    fmt,
    io::{self, Write},
    num::NonZeroU64,
    path::PathBuf,
    time::Duration,
};

use dal_core::{Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace};
use dal_store::{AppendOutcome, Store};

/// A command-line or store failure in the helper process.
#[derive(Debug)]
struct HelperError(String);

impl fmt::Display for HelperError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl std::error::Error for HelperError {}

fn failed(message: impl Into<String>) -> HelperError {
    HelperError(message.into())
}

#[expect(
    clippy::disallowed_methods,
    reason = "test process edge reads its controlled IPC channel"
)]
#[tokio::main(flavor = "current_thread")]
async fn main() -> Result<(), HelperError> {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 5 {
        return Err(failed(
            "usage: append-child <data-root> <workspace-path> <session-id> <hold|append|append-hold> [count]",
        ));
    }
    let data_root = PathBuf::from(&args[1]);
    let workspace =
        Workspace::new(PathBuf::from(&args[2])).map_err(|error| failed(error.to_string()))?;
    let id: SessionId = args[3]
        .parse()
        .map_err(|error| failed(format!("{error:?}")))?;
    let store = Store::new(data_root, workspace, Product::Dalgona);
    let mut session = store.create_session(id);
    session
        .append(vec![Record::Name {
            at: jiff::Timestamp::now(),
            name: Some("child session".into()),
        }])
        .await
        .map_err(|error| failed(error.to_string()))?;
    session
        .append(vec![user_entry(1, "child opens the session")])
        .await
        .map_err(|error| failed(error.to_string()))?;
    match args[4].as_str() {
        "hold" => {
            println!("ready {}", std::process::id());
            let _ = io::stdout().flush();
            std::thread::sleep(Duration::from_secs(3600));
            Ok(())
        }
        "append" => {
            let count: usize = args.get(5).and_then(|text| text.parse().ok()).unwrap_or(10);
            for index in 0..count {
                session
                    .append(vec![Record::Name {
                        at: jiff::Timestamp::now(),
                        name: Some(format!("child batch {index}").into()),
                    }])
                    .await
                    .map_err(|error| failed(error.to_string()))?;
                println!("acked {index}");
            }
            session
                .close()
                .await
                .map_err(|error| failed(error.to_string()))?;
            Ok(())
        }
        "append-hold" => {
            let count: usize = args.get(5).and_then(|text| text.parse().ok()).unwrap_or(3);
            let mut records = Vec::with_capacity(count);
            for index in 0..count {
                let id = u64::try_from(index)
                    .ok()
                    .and_then(|index| index.checked_add(2))
                    .ok_or_else(|| failed("batch record id overflow"))?;
                records.push(user_entry(id, &format!("child batch {index}")));
            }
            match session
                .append(records)
                .await
                .map_err(|error| failed(error.to_string()))?
            {
                AppendOutcome::Durable(_) => {}
                other => return Err(failed(format!("batch was not durable: {other:?}"))),
            }
            println!("acked-batch {count}");
            io::stdout()
                .flush()
                .map_err(|error| failed(error.to_string()))?;

            let mut command = String::new();
            io::stdin()
                .read_line(&mut command)
                .map_err(|error| failed(error.to_string()))?;
            if command.trim() != "hold" {
                return Err(failed("expected the explicit hold command"));
            }
            println!("holding");
            io::stdout()
                .flush()
                .map_err(|error| failed(error.to_string()))?;

            let mut release = String::new();
            io::stdin()
                .read_line(&mut release)
                .map_err(|error| failed(error.to_string()))?;
            Err(failed("hold released unexpectedly"))
        }
        mode => Err(failed(format!("unknown mode: {mode}"))),
    }
}

fn user_entry(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: EntryId::new(NonZeroU64::new(id).unwrap_or(NonZeroU64::MIN)),
        parent: None,
        at: jiff::Timestamp::now(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}
