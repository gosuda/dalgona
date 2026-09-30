use std::{collections::VecDeque, io, path::PathBuf, time::Instant};

use tokio::io::AsyncReadExt;

use super::{
    COMPLETION_TAIL_BYTES, OUTPUT_CHUNK_BYTES, OUTPUT_FILE_CAP_BYTES, PREVIEW_BYTES,
    PROGRESS_LINES, PROGRESS_PERIOD, ProgressFn, TAIL_RING_BYTES, TRUNCATION_MARKER,
};

/// The bounded result of draining both pipes.
#[derive(Debug)]
pub(super) struct CaptureResult {
    pub(super) preview: Box<[u8]>,
    pub(super) completion_tail: Box<[u8]>,
    pub(super) stdout_prefix: Box<[u8]>,
    pub(super) stdout_prefix_overflowed: bool,
    pub(super) denial_seen: bool,
}

/// Drains both pipes in arrival order into one durable log.
pub(super) async fn run_capture(
    mut stdout: tokio::process::ChildStdout,
    mut stderr: tokio::process::ChildStderr,
    log_path: PathBuf,
    stdout_prefix_limit: usize,
    live_tail: tokio::sync::watch::Sender<VecDeque<u8>>,
    progress: Option<ProgressFn>,
) -> Result<CaptureResult, io::Error> {
    use tokio::fs::{self, OpenOptions};

    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent).await?;
    }
    let mut file = OpenOptions::new()
        .create(true)
        .truncate(true)
        .write(true)
        .open(&log_path)
        .await?;
    let marker = TRUNCATION_MARKER.as_bytes();
    let payload_cap = OUTPUT_FILE_CAP_BYTES.saturating_sub(marker.len() as u64);
    let mut kept = 0_u64;
    let mut truncated = false;
    let mut denial_seen = false;
    let mut scan_tail = Vec::new();
    let mut stdout_prefix = Vec::new();
    let mut stdout_prefix_overflowed = false;
    let mut tail = VecDeque::with_capacity(TAIL_RING_BYTES);
    let mut last_progress = Instant::now();
    let mut stdout_done = false;
    let mut stderr_done = false;
    let mut stdout_buf = vec![0_u8; OUTPUT_CHUNK_BYTES];
    let mut stderr_buf = vec![0_u8; OUTPUT_CHUNK_BYTES];

    while !stdout_done || !stderr_done {
        tokio::select! {
            biased;
            read = stdout.read(&mut stdout_buf), if !stdout_done => {
                match read {
                    Ok(0) => stdout_done = true,
                    Ok(n) => {
                        ingest(
                            &mut file,
                            &stdout_buf[..n],
                            true,
                            &mut kept,
                            payload_cap,
                            &mut truncated,
                            &mut denial_seen,
                            &mut scan_tail,
                            &mut stdout_prefix,
                            stdout_prefix_limit,
                            &mut stdout_prefix_overflowed,
                            &mut tail,
                            &live_tail,
                            progress.as_ref(),
                            &mut last_progress,
                        )
                        .await?;
                    }
                    Err(error) => return Err(error),
                }
            }
            read = stderr.read(&mut stderr_buf), if !stderr_done => {
                match read {
                    Ok(0) => stderr_done = true,
                    Ok(n) => {
                        ingest(
                            &mut file,
                            &stderr_buf[..n],
                            false,
                            &mut kept,
                            payload_cap,
                            &mut truncated,
                            &mut denial_seen,
                            &mut scan_tail,
                            &mut stdout_prefix,
                            stdout_prefix_limit,
                            &mut stdout_prefix_overflowed,
                            &mut tail,
                            &live_tail,
                            progress.as_ref(),
                            &mut last_progress,
                        )
                        .await?;
                    }
                    Err(error) => return Err(error),
                }
            }
        }
    }
    file.sync_all().await?;
    Ok(CaptureResult {
        preview: tail_suffix(&tail, PREVIEW_BYTES),
        completion_tail: tail_suffix(&tail, COMPLETION_TAIL_BYTES),
        stdout_prefix: stdout_prefix.into_boxed_slice(),
        stdout_prefix_overflowed,
        denial_seen,
    })
}

#[expect(
    clippy::too_many_arguments,
    reason = "single ingestion site keeps file, truncation, prefix, tail, and progress together"
)]
async fn ingest(
    file: &mut tokio::fs::File,
    bytes: &[u8],
    stdout: bool,
    kept: &mut u64,
    payload_cap: u64,
    truncated: &mut bool,
    denial_seen: &mut bool,
    scan_tail: &mut Vec<u8>,
    stdout_prefix: &mut Vec<u8>,
    stdout_prefix_limit: usize,
    stdout_prefix_overflowed: &mut bool,
    tail: &mut VecDeque<u8>,
    live_tail: &tokio::sync::watch::Sender<VecDeque<u8>>,
    progress: Option<&ProgressFn>,
    last_progress: &mut Instant,
) -> Result<(), io::Error> {
    use tokio::io::AsyncWriteExt;

    *denial_seen |= contains_denial_phrase(scan_tail, bytes);
    if stdout && stdout_prefix.len() < stdout_prefix_limit {
        let take = (stdout_prefix_limit - stdout_prefix.len()).min(bytes.len());
        stdout_prefix.extend_from_slice(&bytes[..take]);
        *stdout_prefix_overflowed |= take < bytes.len();
    } else if stdout && !bytes.is_empty() {
        *stdout_prefix_overflowed = true;
    }

    if !*truncated {
        let remaining = payload_cap.saturating_sub(*kept);
        let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
        let take = remaining.min(bytes.len());
        if take != 0 {
            file.write_all(&bytes[..take]).await?;
            push_tail(tail, &bytes[..take]);
            *kept += u64::try_from(take).unwrap_or(u64::MAX);
        }
        if take < bytes.len() {
            let marker = TRUNCATION_MARKER.as_bytes();
            file.write_all(marker).await?;
            push_tail(tail, marker);
            *kept += u64::try_from(marker.len()).unwrap_or(u64::MAX);
            *truncated = true;
        }
    }

    if let Some(callback) = progress
        && last_progress.elapsed() >= PROGRESS_PERIOD
    {
        let snapshot: Vec<u8> = tail.iter().copied().collect();
        callback(super::last_lines(&snapshot, PROGRESS_LINES).into_boxed_str());
        *last_progress = Instant::now();
    }
    let _ = live_tail.send(tail.clone());
    Ok(())
}

pub(super) fn push_tail(tail: &mut VecDeque<u8>, bytes: &[u8]) {
    for byte in bytes {
        if tail.len() == TAIL_RING_BYTES {
            tail.pop_front();
        }
        tail.push_back(*byte);
    }
}

pub(super) fn tail_suffix(tail: &VecDeque<u8>, max_bytes: usize) -> Box<[u8]> {
    let skip = tail.len().saturating_sub(max_bytes);
    tail.iter().skip(skip).copied().collect()
}

pub(super) fn contains_denial_phrase(scan_tail: &mut Vec<u8>, bytes: &[u8]) -> bool {
    const MARKERS: [&[u8]; 2] = [b"Permission denied", b"Operation not permitted"];
    let mut combined = Vec::with_capacity(scan_tail.len() + bytes.len());
    combined.extend_from_slice(scan_tail);
    combined.extend_from_slice(bytes);
    let old_len = scan_tail.len();
    let found = MARKERS.iter().any(|marker| {
        combined
            .windows(marker.len())
            .enumerate()
            .any(|(start, window)| window == *marker && start + marker.len() > old_len)
    });
    let keep = combined.len().min(32);
    scan_tail.clear();
    scan_tail.extend_from_slice(&combined[combined.len() - keep..]);
    found
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    #[test]
    fn denial_marker_detection_spans_chunks() {
        let mut tail = Vec::new();
        assert!(!contains_denial_phrase(&mut tail, b"Permission "));
        assert!(contains_denial_phrase(&mut tail, b"denied"));
        assert!(!contains_denial_phrase(&mut tail, b"ordinary output"));
    }

    #[test]
    fn output_tail_stays_bounded() {
        let mut tail = VecDeque::new();
        let large_chunk = vec![b'a'; TAIL_RING_BYTES + 8];
        push_tail(&mut tail, &large_chunk);
        assert_eq!(tail.len(), TAIL_RING_BYTES);
        assert_eq!(tail_suffix(&tail, 4).as_ref(), b"aaaa");
    }
}
