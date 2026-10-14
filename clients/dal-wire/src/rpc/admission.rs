//! Weighted, process-wide admission for owned RPC request payloads.
//!
//! A frame is charged before its transport reads any bytes. After receiving a
//! frame, unused credit is returned and the remainder follows its handler.
//! Reserving a maximum frame first also bounds simultaneous transport read
//! buffers across connections. The weight leaves room for the decoded JSON
//! tree, which can allocate substantially more than the wire representation.

use std::sync::{Arc, LazyLock};

use tokio::sync::{AcquireError, OwnedSemaphorePermit, Semaphore};

use crate::transport::MAX_FRAME_BYTES;

/// Four charged bytes per inbound wire byte cover raw and decoded ownership.
const BYTE_WEIGHT: usize = 4;
/// Must cover the maximum inbound frame, including its estimated decode cost.
const FRAME_RESERVATION: u32 = 64 * 1024 * 1024;
/// At most two maximum-size requests are retained by a single connection.
const CONNECTION_LIMIT: usize = 2 * FRAME_RESERVATION as usize;
/// At most four maximum-size requests are retained across all connections.
const PROCESS_LIMIT: usize = 4 * FRAME_RESERVATION as usize;

const _: () = assert!(FRAME_RESERVATION as usize == MAX_FRAME_BYTES * BYTE_WEIGHT);

static PROCESS_BUDGET: LazyLock<Arc<Semaphore>> =
    LazyLock::new(|| Arc::new(Semaphore::new(PROCESS_LIMIT)));

/// Shared process credit and private per-connection credit.
pub(super) struct Admission {
    process: Arc<Semaphore>,
    connection: Arc<Semaphore>,
}

impl Admission {
    pub(super) fn new() -> Self {
        Self {
            process: Arc::clone(&PROCESS_BUDGET),
            connection: Arc::new(Semaphore::new(CONNECTION_LIMIT)),
        }
    }

    /// Reserves enough credit before reading a potentially maximum-size frame.
    pub(super) async fn reserve(&self) -> Result<FrameCredit, AcquireError> {
        let connection = Arc::clone(&self.connection)
            .acquire_many_owned(FRAME_RESERVATION)
            .await?;
        let process = Arc::clone(&self.process)
            .acquire_many_owned(FRAME_RESERVATION)
            .await?;
        Ok(FrameCredit {
            connection,
            process,
        })
    }
}

/// Both budgets are held through parsing and until the handler is dropped.
pub(super) struct FrameCredit {
    connection: OwnedSemaphorePermit,
    process: OwnedSemaphorePermit,
}

impl FrameCredit {
    /// Returns unused reservation credit once the actual frame size is known.
    ///
    /// Preconditions: the transport has enforced `MAX_FRAME_BYTES`.
    pub(super) fn charge(&mut self, frame_bytes: usize) {
        assert!(frame_bytes <= MAX_FRAME_BYTES);
        let charge = frame_bytes.max(1) * BYTE_WEIGHT;
        let refund = FRAME_RESERVATION as usize - charge;
        assert!(self.connection.split(refund).is_some());
        assert!(self.process.split(refund).is_some());
    }
}

#[cfg(test)]
mod tests {
    use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
    use tokio::sync::oneshot;

    use super::*;

    fn isolated(process: &Arc<Semaphore>, connection_limit: usize) -> Admission {
        Admission {
            process: Arc::clone(process),
            connection: Arc::new(Semaphore::new(connection_limit)),
        }
    }

    #[tokio::test]
    async fn maximum_frames_are_bounded_per_connection() {
        let process = Arc::new(Semaphore::new(PROCESS_LIMIT));
        let admission = isolated(&process, CONNECTION_LIMIT);
        let first = admission.reserve().await.expect("first reservation");
        let second = admission.reserve().await.expect("second reservation");
        assert!(
            admission.reserve().now_or_never().is_none(),
            "a third maximum-size frame must not be read"
        );
        drop(first);
        let third = admission.reserve().await.expect("credit released");
        drop(second);
        drop(third);
        assert_eq!(process.available_permits(), PROCESS_LIMIT);
    }

    #[tokio::test]
    async fn credit_follows_its_exact_handler_until_completion() {
        let process = Arc::new(Semaphore::new(FRAME_RESERVATION as usize));
        let admission = isolated(&process, FRAME_RESERVATION as usize);
        let mut credit = admission.reserve().await.expect("frame credit");
        credit.charge(MAX_FRAME_BYTES - 128);
        assert_eq!(process.available_permits(), 128 * BYTE_WEIGHT);

        let (started_tx, started_rx) = oneshot::channel();
        let (finish_tx, finish_rx) = oneshot::channel::<()>();
        let handler = async move {
            let _credit = credit;
            let _ = started_tx.send(());
            let _ = finish_rx.await;
        }
        .boxed();
        let mut pending = FuturesUnordered::new();
        pending.push(handler);
        tokio::select! {
            () = async { started_rx.await.expect("handler started") } => {}
            _ = pending.next() => panic!("handler completed before release"),
        }
        assert!(
            admission.reserve().now_or_never().is_none(),
            "an unfinished handler still owns the frame"
        );
        finish_tx.send(()).expect("finish handler");
        assert!(pending.next().await.is_some());
        assert_eq!(process.available_permits(), FRAME_RESERVATION as usize);
    }

    #[tokio::test]
    async fn one_connection_cannot_borrow_another_connections_budget() {
        let process = Arc::new(Semaphore::new(FRAME_RESERVATION as usize));
        let first = isolated(&process, CONNECTION_LIMIT);
        let second = isolated(&process, CONNECTION_LIMIT);
        let held = first.reserve().await.expect("first peer");
        assert!(
            second.reserve().now_or_never().is_none(),
            "the shared process budget stops a second peer"
        );
        drop(held);
        let resumed = second.reserve().await.expect("second peer resumes");
        drop(resumed);
        assert_eq!(process.available_permits(), FRAME_RESERVATION as usize);
    }

    #[tokio::test]
    async fn small_frames_refund_unused_global_and_connection_credit() {
        let process = Arc::new(Semaphore::new(PROCESS_LIMIT));
        let admission = isolated(&process, CONNECTION_LIMIT);
        let mut held = admission.reserve().await.expect("credit");
        held.charge(64);
        assert_eq!(process.available_permits(), PROCESS_LIMIT - 64 * BYTE_WEIGHT);
        assert_eq!(
            admission.connection.available_permits(),
            CONNECTION_LIMIT - 64 * BYTE_WEIGHT
        );
        drop(held);
        assert_eq!(process.available_permits(), PROCESS_LIMIT);
        assert_eq!(admission.connection.available_permits(), CONNECTION_LIMIT);
    }
}
