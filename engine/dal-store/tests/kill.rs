#![cfg_attr(
    unix,
    expect(clippy::expect_used, reason = "integration tests fail loudly")
)]
#![cfg_attr(
    unix,
    expect(clippy::disallowed_methods, reason = "integration tests fail loudly")
)]
//! Kill recovery: a post-receipt SIGKILL preserves an acknowledged batch.

#![cfg(unix)]

mod support;

use std::{
    io::{BufRead, BufReader, Write},
    num::NonZeroU64,
    os::unix::process::ExitStatusExt,
    process::{Child, ChildStdin, Command, ExitStatus, Stdio},
    sync::mpsc::{self, Receiver},
    thread::JoinHandle,
    time::Duration,
};

use dal_core::{Entry, EntryId, EntryKind, JournalPart, Product, Record, SessionId, Workspace};
use dal_store::Store;
use support::temp_dir::TempDir;

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);
const BATCH_RECORDS: usize = 3;

struct OwnedChild {
    child: Child,
    stdin: Option<ChildStdin>,
    output_reader: Option<JoinHandle<()>>,
    reaped: bool,
}

impl OwnedChild {
    fn is_running(&mut self) -> bool {
        self.child
            .try_wait()
            .expect("helper status is readable")
            .is_none()
    }

    fn kill_and_reap(&mut self) -> ExitStatus {
        if self.is_running() {
            self.child.kill().expect("owned helper receives SIGKILL");
        }
        let _ = self.stdin.take();
        let status = self.child.wait().expect("owned helper is reaped");
        self.reaped = true;
        self.join_output();
        status
    }

    fn join_output(&mut self) {
        if let Some(reader) = self.output_reader.take() {
            let _ = reader.join();
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            let running = self
                .child
                .try_wait()
                .map_or(true, |status| status.is_none());
            if running {
                let _ = self.child.kill();
            }
            let _ = self.stdin.take();
            let _ = self.child.wait();
            self.reaped = true;
        }
        self.join_output();
    }
}

fn next_line(receiver: &Receiver<std::io::Result<String>>, label: &str) -> String {
    receiver
        .recv_timeout(HANDSHAKE_TIMEOUT)
        .expect(label)
        .expect(label)
}

fn user_entry(id: u64, text: &str) -> Record {
    Record::User(Entry {
        id: EntryId::new(NonZeroU64::new(id).expect("test entry id is nonzero")),
        parent: None,
        at: jiff::Timestamp::now(),
        kind: EntryKind::User {
            parts: vec![JournalPart::Text { text: text.into() }],
        },
    })
}

fn user_rows(records: &[Record]) -> Vec<(u64, String)> {
    records
        .iter()
        .filter_map(|record| {
            let Record::User(entry) = record else {
                return None;
            };
            let EntryKind::User { parts } = &entry.kind else {
                return None;
            };
            let [JournalPart::Text { text }] = parts.as_slice() else {
                return None;
            };
            Some((entry.id.get(), text.to_string()))
        })
        .collect()
}

fn spawn_acknowledged_helper(
    data_root: &std::path::Path,
    workspace: &Workspace,
    id: SessionId,
) -> (OwnedChild, Receiver<std::io::Result<String>>) {
    let mut process = Command::new(env!("CARGO_BIN_EXE_append-child"))
        .arg(data_root)
        .arg(workspace.as_path())
        .arg(id.to_string())
        .arg("append-hold")
        .arg(BATCH_RECORDS.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("helper process spawns");
    let stdin = process.stdin.take().expect("helper stdin is piped");
    let stdout = process.stdout.take().expect("helper stdout is piped");
    let mut child = OwnedChild {
        stdin: Some(stdin),
        output_reader: None,
        child: process,
        reaped: false,
    };
    let (sender, receiver) = mpsc::channel();
    child.output_reader = Some(std::thread::spawn(move || {
        for line in BufReader::new(stdout).lines() {
            if sender.send(line).is_err() {
                break;
            }
        }
    }));
    (child, receiver)
}

fn reopen_and_check_recovered(store: &Store, id: SessionId) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    runtime.block_on(async {
        let (mut reopened, report) = store
            .open_session(id)
            .await
            .expect("reopen after post-receipt SIGKILL succeeds");
        assert!(
            report.torn.is_none(),
            "the acknowledged boundary leaves no torn tail"
        );
        assert!(report.aborted.is_none(), "the helper opens no turn");
        let mut expected = vec![(1, "child opens the session".to_owned())];
        expected.extend(
            (0..BATCH_RECORDS).map(|index| (index as u64 + 2, format!("child batch {index}"))),
        );
        assert_eq!(
            user_rows(reopened.records()),
            expected,
            "every exact record in the acknowledged batch survives reopen"
        );
        reopened
            .append(vec![user_entry(5, "after acknowledged crash")])
            .await
            .expect("reopened session remains usable");
        reopened.close().await.expect("recovered session closes");
        let (mut reopened_again, report_again) = store
            .open_session(id)
            .await
            .expect("second reopen after recovered append succeeds");
        assert!(
            report_again.torn.is_none(),
            "recovered append leaves no torn tail on the next reopen"
        );
        expected.push((5, "after acknowledged crash".to_owned()));
        assert_eq!(
            user_rows(reopened_again.records()),
            expected,
            "the acknowledged batch and recovered append remain exact"
        );
        reopened_again
            .close()
            .await
            .expect("second recovered session closes");
    });
}

#[test]
fn sigkill_after_acknowledged_batch_recovers() {
    let temp = TempDir::new("store-kill");
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let data_root = temp.path().join("data");
    let store = Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
    let id = SessionId::new_v7();
    let (mut child, receiver) = spawn_acknowledged_helper(&data_root, &workspace, id);
    let acknowledged = next_line(&receiver, "acknowledged batch");
    assert_eq!(
        acknowledged, "acked-batch 3",
        "the helper acknowledges only after one durable multi-record append"
    );
    {
        let stdin = child.stdin.as_mut().expect("helper stdin remains open");
        writeln!(stdin, "hold").expect("hold command reaches helper");
        stdin.flush().expect("hold command flushes");
    }
    assert_eq!(next_line(&receiver, "hold state"), "holding");
    assert!(
        child.is_running(),
        "the acknowledged helper remains alive at the explicit IPC hold"
    );
    let status = child.kill_and_reap();
    assert_eq!(
        status.signal(),
        Some(9),
        "the owned helper must die from the requested SIGKILL"
    );
    reopen_and_check_recovered(&store, id);
}
