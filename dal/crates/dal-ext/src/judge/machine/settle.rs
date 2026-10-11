use std::sync::Arc;

use dal_core::TurnId;
use tokio::time::Instant;

use super::super::record::JudgeRow;
use super::super::types::JudgeError;
use super::{JudgeInner, elapsed_ms, provider_error};

pub(super) struct CallSettlement<'a> {
    inner: Arc<JudgeInner>,
    call: u64,
    turn: Option<TurnId>,
    feature: &'a str,
    questions: usize,
    started: Instant,
    slot_wait_started: Option<Instant>,
    slot_wait_ms: u64,
    settled: bool,
}

impl<'a> CallSettlement<'a> {
    pub(super) fn new(
        inner: Arc<JudgeInner>,
        call: u64,
        turn: Option<TurnId>,
        feature: &'a str,
        questions: usize,
    ) -> Self {
        Self {
            inner,
            call,
            turn,
            feature,
            questions,
            started: Instant::now(),
            slot_wait_started: None,
            slot_wait_ms: 0,
            settled: false,
        }
    }

    pub(super) fn start_slot_wait(&mut self) {
        self.slot_wait_started = Some(Instant::now());
    }

    pub(super) fn finish_slot_wait(&mut self) {
        if let Some(started) = self.slot_wait_started.take() {
            self.slot_wait_ms = elapsed_ms(started.elapsed());
        }
    }

    pub(super) fn row(
        &self,
        status: &'static str,
        cause: &str,
        duration_ms: u64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> JudgeRow {
        JudgeRow::new(
            self.call,
            self.turn,
            self.feature,
            self.questions,
            status,
            cause,
            &self.inner.model_id,
            duration_ms,
            self.slot_wait_ms,
            input_tokens,
            output_tokens,
        )
    }

    pub(super) async fn append(&mut self, row: JudgeRow) -> Result<(), JudgeError> {
        let body = row.into_raw()?;
        // Mark the call settled before awaiting: a failed append is the
        // call's own terminal attempt, never a second `cancelled` row from
        // `Drop`. A row lost here is the declared close-race recovery.
        self.settled = true;
        self.inner
            .services
            .append_record(&self.inner.caller, "judge", Box::new(body))
            .await
            .map_err(|error| provider_error(&error))
            .map(|_| ())?;
        Ok(())
    }
}

impl Drop for CallSettlement<'_> {
    fn drop(&mut self) {
        if self.settled {
            return;
        }
        let slot_wait_ms = self
            .slot_wait_started
            .map_or(self.slot_wait_ms, |started| elapsed_ms(started.elapsed()));
        let row = JudgeRow::new(
            self.call,
            self.turn,
            self.feature,
            self.questions,
            "cancelled",
            "cancelled",
            &self.inner.model_id,
            elapsed_ms(self.started.elapsed()),
            slot_wait_ms,
            0,
            0,
        );
        let body = match row.into_raw() {
            Ok(body) => body,
            Err(error) => {
                tracing::error!(%error, call = self.call, "failed to serialize cancelled judge row");
                return;
            }
        };
        let services = Arc::clone(&self.inner.services);
        let caller = self.inner.caller.clone();
        let call = self.call;
        match tokio::runtime::Handle::try_current() {
            Ok(runtime) => {
                drop(runtime.spawn(async move {
                    if let Err(error) = services
                        .append_record(&caller, "judge", Box::new(body))
                        .await
                    {
                        tracing::error!(%error, call, "failed to append cancelled judge row");
                    }
                }));
            }
            Err(error) => {
                tracing::error!(%error, call = self.call, "cancelled judge row has no runtime");
            }
        }
    }
}
