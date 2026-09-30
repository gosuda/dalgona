//! Reaps detached processes into the one session job table.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use dal_core::{JobId, JobOutcome};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::sync::Mutex;
use tokio::time::{Instant, interval_at};
use tokio_util::sync::CancellationToken;

use crate::proc::{Proc, StopReason};

use super::JobTable;

/// Waits for one detached process, streams bounded output lines, and records
/// its unique terminal outcome.
pub(crate) async fn reap_detached(
    mut process: Proc,
    table: Arc<Mutex<JobTable>>,
    cancel: CancellationToken,
) {
    let id: JobId = process.job_id();
    let log_path = process.log_path().to_path_buf();
    let mut offset = 0_u64;
    let mut tick = interval_at(
        Instant::now() + Duration::from_millis(100),
        Duration::from_millis(100),
    );
    let result = {
        let wait = process.wait(&cancel);
        tokio::pin!(wait);
        loop {
            tokio::select! {
                result = &mut wait => break result,
                _ = tick.tick() => {
                    if let Ok(bytes) = read_new(&log_path, &mut offset).await
                        && !bytes.is_empty()
                    {
                        table.lock().await.push_output(id, &bytes);
                    }
                }
            }
        }
    };
    let tail = read_new(&log_path, &mut offset).await.unwrap_or_default();
    let (outcome, completion_tail) = match result {
        Ok(result) => (result.outcome, result.completion_tail),
        Err(error) => {
            let _ = process.stop(StopReason::Cancelled).await;
            (
                JobOutcome::Failed {
                    message: error.to_string().into(),
                },
                Vec::new().into_boxed_slice(),
            )
        }
    };
    let mut table = table.lock().await;
    if !tail.is_empty() {
        table.push_output(id, &tail);
    }
    let _ = table.settle_once(id, outcome, completion_tail);
    let _ = table.flush().await;
}

async fn read_new(path: &Path, offset: &mut u64) -> Result<Vec<u8>, io::Error> {
    let mut file = match tokio::fs::OpenOptions::new().read(true).open(path).await {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    file.seek(std::io::SeekFrom::Start(*offset)).await?;
    let mut bytes = vec![0; 8192];
    let read = file.read(&mut bytes).await?;
    bytes.truncate(read);
    let read = u64::try_from(read).map_err(|_| io::Error::other("output length overflow"))?;
    *offset = offset.saturating_add(read);
    Ok(bytes)
}
