#![cfg_attr(
    unix,
    expect(clippy::expect_used, reason = "integration tests fail loudly")
)]
#![cfg_attr(
    unix,
    expect(clippy::disallowed_methods, reason = "integration tests fail loudly")
)]
//! Kill recovery: SIGKILL at random points never loses an acknowledged batch.

#![cfg(unix)]

mod support;

use std::{
    process::{Command, Stdio},
    time::{SystemTime, UNIX_EPOCH},
};

use dal_core::{Product, Record, SessionId, Workspace};
use dal_store::{Store, StoreError};
use support::temp_dir::TempDir;

fn nanos() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock runs")
        .subsec_nanos()
        .into()
}

#[test]
#[ignore = "SIGKILL timing is nondeterministic; run explicitly"]
fn sigkill_survives_every_reopen() {
    let temp = TempDir::new("store-kill");
    let workspace =
        Workspace::new(temp.path().join("workspace")).expect("temporary workspace is absolute");
    let data_root = temp.path().join("data");
    let store = Store::new(data_root.clone(), workspace.clone(), Product::Dalgona);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    for round in 0..200 {
        let id = SessionId::new_v7();
        let mut child = Command::new(env!("CARGO_BIN_EXE_append-child"))
            .arg(&data_root)
            .arg(workspace.as_path())
            .arg(id.to_string())
            .arg("append")
            .arg("5")
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("helper process spawns");
        if !nanos().is_multiple_of(3) {
            std::thread::sleep(std::time::Duration::from_micros(nanos() % 15_000));
            let _ = child.kill();
        }
        let output = child.wait_with_output().expect("helper is reaped");
        let printed: Vec<usize> = String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| {
                line.strip_prefix("acked ")
                    .and_then(|rest| rest.parse().ok())
            })
            .collect();
        runtime.block_on(async {
            match store.open_session(id).await {
                Ok((reopened, report)) => {
                    for index in &printed {
                        let want = format!("child batch {index}");
                        assert!(
                            reopened.records().iter().any(|record| match record {
                                Record::Name { name, .. } => {
                                    name.as_deref() == Some(want.as_str())
                                }
                                _ => false,
                            }),
                            "round {round}: every printed acknowledged id exists"
                        );
                    }
                    if let Some(torn) = report.torn {
                        assert!(
                            torn.kept_at.is_file(),
                            "round {round}: every torn tail is quarantined"
                        );
                    }
                }
                Err(StoreError::NotFound { .. }) => {
                    assert!(
                        printed.is_empty(),
                        "round {round}: acknowledged batches are always durable"
                    );
                }
                Err(other) => panic!("round {round}: every reopen succeeds, got {other:?}"),
            }
        });
    }
}
