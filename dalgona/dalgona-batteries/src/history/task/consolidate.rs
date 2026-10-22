// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! Dream consolidation: jobs lifecycle, judge call, record commit, recount.
//!
//! The second half of the session task: everything that runs after a
//! `SubmitJob` action, plus the record-derived counts both triggers share.
//! The shared context carries each selected letter's stored text through
//! `letter://`, oldest first with id labels, prefix-fit to the judge budget.
//! A context that cannot be built fails with a typed, sanitized cause:
//! budget exhaustion and unresolvable letter text are non-retryable, a
//! cancelled read is neutral, and any other service failure is transient.

use std::num::NonZeroU64;
use std::sync::Arc;

use dal_agent::error::ServiceError;
use dal_core::ext::{JobsOp, JobsReply, Name, SidecarName, SidecarOp};
use dal_core::{EntryId, RawJson};
use dal_ext::judge::{Gate, Judge, JudgeError, JudgeOpen, SHARED_MAX};
use jiff::{Timestamp, Unit};
use serde::Serialize;
use tokio::sync::mpsc;

use super::super::dream::{DreamEvent, DreamFile, DreamPhase, ParkFile, consolidated_notice};
use super::super::records::LetterRecord;
use super::super::{DREAM_JOB_NAME, DREAM_PROMPT, DREAM_SIDECAR_NAME};
use super::{JobMode, Task, TaskMsg};

/// Fixed notice when the session carries no sidecar service (ephemeral).
const EPHEMERAL_NOTICE: &str = "history: auto-dream is unavailable in an ephemeral session.";

/// Non-retryable text: a selected letter resolves to no stored text.
const SOURCE_MISSING: &str = "history: a selected letter has no stored text.";

/// Non-retryable text: the host failed to read a selected letter's text.
const SOURCE_UNREADABLE: &str = "history: a selected letter's stored text could not be read.";

/// One dream letter record body for the journal write.
#[derive(Serialize)]
struct DreamBody {
    v: u8,
    id: String,
    kind: &'static str,
    letters: Vec<String>,
    summary: String,
}

/// One unreflected letter awaiting consolidation.
#[derive(Clone)]
pub(super) struct BatchLetter {
    id: String,
}

impl Task {
    /// Submits one dream job and consolidates the unreflected batch inline.
    pub(super) async fn submit(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        Box::pin(self.submit_with_mode(rx, JobMode::Normal)).await;
    }

    /// Submits one half-open probe job.
    pub(super) async fn submit_probe(&mut self, rx: &mut mpsc::Receiver<TaskMsg>) {
        self.submit_with_mode(rx, JobMode::Probe).await;
    }

    async fn submit_with_mode(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, mode: JobMode) {
        if self.unavailable {
            self.fire(rx, DreamEvent::JobCancelled).await;
            self.notify(EPHEMERAL_NOTICE.to_string());
            return;
        }
        let payload = format!(r#"{{"session":"{}"}}"#, self.session);
        let parsed = match (Name::parse(DREAM_JOB_NAME), RawJson::parse(&payload)) {
            (Ok(name), Ok(payload)) => Some((name, payload)),
            _ => None,
        };
        let Some((job_name, job_payload)) = parsed else {
            self.fail_transient(rx, mode).await;
            return;
        };
        match self
            .services
            .jobs(
                &self.caller,
                JobsOp::Spawn {
                    name: job_name,
                    payload: job_payload,
                    parent: None,
                },
            )
            .await
        {
            Err(ServiceError::Cancelled) => {
                self.fire(rx, DreamEvent::JobCancelled).await;
            }
            Ok(JobsReply::Spawned { id }) => {
                self.job = Some(id);
                self.consolidate(rx, mode).await;
            }
            Err(_) | Ok(_) => {
                self.fail_transient(rx, mode).await;
            }
        }
    }

    /// Consolidates the current batch through one typed judge call.
    async fn consolidate(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, mode: JobMode) {
        self.recount().await;
        if self.batch.is_empty() {
            self.fire(rx, DreamEvent::JobCancelled).await;
            self.cancel_job().await;
            return;
        }
        let Some(judge) = self.ensure_judge().await else {
            self.finish_gate_off(rx, mode).await;
            return;
        };
        if matches!(judge.state(), Gate::Off) {
            self.finish_gate_off(rx, mode).await;
            return;
        }
        let (shared, fitted) = match self.shared_context().await {
            Ok(ready) => ready,
            Err(error) => {
                self.finish_context_error(rx, error, mode).await;
                return;
            }
        };
        match judge.summarize(&shared, DREAM_PROMPT).await {
            Ok(summary) => self.commit(rx, summary, mode, fitted).await,
            Err(error) => self.finish_judge_error(rx, error, mode).await,
        }
    }

    /// Opens or reuses the session judge; `None` means no model call is possible.
    async fn ensure_judge(&mut self) -> Option<Judge> {
        if let Some(judge) = &self.judge {
            return Some(judge.clone());
        }
        let open = JudgeOpen {
            session: self.session,
            config: self.judge_config.clone(),
            services: Arc::clone(&self.services),
            caller: self.caller.clone(),
            session_route: self.session_route.clone(),
            session_model_id: self.session_model_id.clone(),
        };
        let judge = Judge::open(open).await.ok()?;
        self.judge = Some(judge.clone());
        Some(judge)
    }

    /// Handles a gate-off or unopenable judge: no call, streaks kept, batch stays.
    async fn finish_gate_off(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, mode: JobMode) {
        self.fire(rx, mode.gate_off()).await;
        self.cancel_job().await;
    }

    /// Classifies one judge failure into non-retryable, transient, or neutral outcomes.
    async fn finish_judge_error(
        &mut self,
        rx: &mut mpsc::Receiver<TaskMsg>,
        error: JudgeError,
        mode: JobMode,
    ) {
        if matches!(mode, JobMode::Probe) {
            self.fire(rx, mode.failure()).await;
            self.cancel_job().await;
            return;
        }
        let event = match error {
            JudgeError::Parse { .. }
            | JudgeError::InvalidQuestion { .. }
            | JudgeError::SharedTooLarge { .. } => {
                DreamEvent::JobIdenticalFailure(error.to_string())
            }
            JudgeError::BudgetExhausted { .. } => DreamEvent::BudgetExhausted,
            JudgeError::Denied | JudgeError::Unavailable => DreamEvent::JudgeOff,
            _ => DreamEvent::JobTransientFailure,
        };
        self.fire(rx, event).await;
        self.cancel_job().await;
    }

    /// Classifies one context failure into neutral, non-retryable, or
    /// transient outcomes; the batch stays unreflected in every case.
    ///
    /// A budget failure reuses the judge's own non-retryable
    /// [`JudgeError::SharedTooLarge`] text, so it is the failure the judge
    /// would have reported for the same context. Texts carry no letter id
    /// and no stored text.
    async fn finish_context_error(
        &mut self,
        rx: &mut mpsc::Receiver<TaskMsg>,
        error: ContextError,
        mode: JobMode,
    ) {
        let event = match error {
            ContextError::Cancelled => DreamEvent::JobCancelled,
            ContextError::Transient => mode.failure(),
            ContextError::TooLarge { len } => {
                mode.non_retryable(JudgeError::SharedTooLarge { len }.to_string())
            }
            ContextError::SourceMissing => mode.non_retryable(SOURCE_MISSING.to_owned()),
            ContextError::SourceUnreadable => mode.non_retryable(SOURCE_UNREADABLE.to_owned()),
        };
        self.fire(rx, event).await;
        self.cancel_job().await;
    }

    /// Fires the transient failure for the current job mode.
    async fn fail_transient(&mut self, rx: &mut mpsc::Receiver<TaskMsg>, mode: JobMode) {
        self.fire(rx, mode.failure()).await;
    }

    /// Appends the dream record, then marks consumed and rewrites the sidecar.
    ///
    /// Only the fitted ids whose text reached the shared context are
    /// consumed; the remainder stays unreflected for a later run.
    async fn commit(
        &mut self,
        rx: &mut mpsc::Receiver<TaskMsg>,
        summary: String,
        mode: JobMode,
        fitted: Vec<String>,
    ) {
        let ordinal = self.dreams.saturating_add(1);
        let count = u32::try_from(fitted.len()).unwrap_or(u32::MAX);
        let body = DreamBody {
            v: 1,
            id: format!("dream/{ordinal}"),
            kind: "dream",
            letters: fitted,
            summary,
        };
        let Ok(text) = sonic_rs::to_string(&body) else {
            self.fail_transient(rx, mode).await;
            self.cancel_job().await;
            return;
        };
        let Ok(raw) = RawJson::parse(&text) else {
            self.fail_transient(rx, mode).await;
            self.cancel_job().await;
            return;
        };
        if self.note_end_arrived(rx) {
            self.ended_flag = true;
            self.on_end(rx).await;
            return;
        }
        match self
            .services
            .append_record(&self.caller, "letter", Box::new(raw))
            .await
        {
            Err(ServiceError::Cancelled) => {
                self.fire(rx, DreamEvent::JobCancelled).await;
            }
            Err(_) => {
                self.fail_transient(rx, mode).await;
            }
            Ok(_) => {
                self.fire(rx, mode.success()).await;
                self.recount().await;
                self.write_sidecar().await;
                self.notify(consolidated_notice(count));
            }
        }
        self.cancel_job().await;
    }

    /// Rebuilds the consumed set and unreflected batch from own records.
    ///
    /// Records are the source of truth: a record wins over a stale sidecar,
    /// and removing a dream record on a branch restores its source letters.
    /// Bodies the resolver would reject are skipped here; validation stays
    /// with the `letter://` resolver. Entry ordering inside stored records
    /// is unavailable through this service surface, so span earlier-entry
    /// checks run against the maximum entry.
    pub(super) async fn recount(&mut self) {
        let bodies = self
            .services
            .records(&self.caller, "letter")
            .await
            .unwrap_or_default();
        let max_entry = EntryId::new(NonZeroU64::MAX);
        let mut sequence: Vec<LetterRecord> = Vec::new();
        for body in &bodies {
            if let Ok(record) = LetterRecord::decode(body, max_entry) {
                sequence.push(record);
            }
        }
        let mut consumed: Vec<String> = Vec::new();
        let mut summaries: Vec<String> = Vec::new();
        let mut dreams: u32 = 0;
        for record in &sequence {
            if let LetterRecord::Dream {
                letters, summary, ..
            } = record
            {
                consumed.extend(letters.iter().cloned());
                summaries.push(summary.clone());
                dreams = dreams.saturating_add(1);
            }
        }
        let mut batch: Vec<BatchLetter> = Vec::new();
        let mut last = self.last_consolidated;
        for (index, record) in sequence.iter().enumerate() {
            let position = u64::try_from(index).unwrap_or(u64::MAX).saturating_add(1);
            if matches!(record, LetterRecord::Dream { .. }) {
                continue;
            }
            let listed = consumed.iter().any(|id| id.as_str() == record.id());
            if position > self.last_consolidated && !listed {
                batch.push(BatchLetter {
                    id: record.id().to_string(),
                });
            } else {
                last = last.max(position);
            }
        }
        for id in &consumed {
            if let Some(position) = sequence
                .iter()
                .position(|record| record.id() == id.as_str())
            {
                let position = u64::try_from(position)
                    .unwrap_or(u64::MAX)
                    .saturating_add(1);
                last = last.max(position);
            }
        }
        self.last_consolidated = last;
        self.dreams = dreams;
        self.summaries = summaries;
        self.state.unreflected = batch.len();
        self.batch = batch;
    }

    /// Resolves every batch letter's stored text through `letter://` and
    /// renders the bounded judge shared context, oldest first.
    ///
    /// Each fitted letter contributes its documented id label plus its exact
    /// stored text after the carried summaries. Letters past the shared
    /// budget stay unreflected for a later run, so the prompt never grows
    /// without bound and the commit consumes only fitted ids.
    ///
    /// A non-empty batch yields at least one fitted id on success. The oldest
    /// letter is never truncated, skipped, or split across calls: when it
    /// cannot fit with the carried summaries and header, the whole context
    /// fails with [`ContextError::TooLarge`] and nothing is consumed. A later
    /// letter that does not fit ends the prefix instead.
    ///
    /// # Errors
    /// [`ContextError::TooLarge`] when the carried summaries and header, or
    /// the oldest letter's block, already exceed the budget;
    /// [`ContextError::SourceMissing`] when a letter resolves to no text;
    /// [`ContextError::SourceUnreadable`] when the host reports a read
    /// failure; [`ContextError::Cancelled`] when the read is cancelled; and
    /// [`ContextError::Transient`] for any other service failure.
    async fn shared_context(&self) -> Result<(String, Vec<String>), ContextError> {
        let mut shared = String::new();
        for summary in &self.summaries {
            shared.push_str(summary);
            shared.push_str("\n\n");
        }
        shared.push_str("Unreflected letters (oldest first):\n");
        if shared.len() > SHARED_MAX {
            return Err(ContextError::TooLarge { len: shared.len() });
        }
        let mut fitted: Vec<String> = Vec::new();
        for letter in &self.batch {
            let uri = format!("letter://{}", letter.id);
            let doc = match self.services.scheme(&self.caller, &uri).await {
                Ok(Some(doc)) => doc,
                Ok(None) => return Err(ContextError::SourceMissing),
                Err(ServiceError::Cancelled) => return Err(ContextError::Cancelled),
                Err(ServiceError::Failed { .. }) => return Err(ContextError::SourceUnreadable),
                Err(_) => return Err(ContextError::Transient),
            };
            let len = shared
                .len()
                .saturating_add(uri.len())
                .saturating_add(doc.text.len())
                .saturating_add(2);
            if len > SHARED_MAX {
                if fitted.is_empty() {
                    return Err(ContextError::TooLarge { len });
                }
                break;
            }
            shared.push_str(&uri);
            shared.push('\n');
            shared.push_str(&doc.text);
            shared.push('\n');
            fitted.push(letter.id.clone());
        }
        Ok((shared, fitted))
    }

    /// Atomically rewrites `dream.json` from live state.
    pub(super) async fn write_sidecar(&self) {
        let Ok(name) = SidecarName::parse(DREAM_SIDECAR_NAME) else {
            return;
        };
        let unreflected = u32::try_from(self.state.unreflected).unwrap_or(u32::MAX);
        let file = DreamFile {
            v: 1,
            session: self.session.to_string(),
            last_consolidated_letter: self.last_consolidated,
            unreflected,
            park: ParkFile {
                parked: self.state.phase == DreamPhase::Parked,
                identical_streak: self.state.identical_streak,
                transient_streak: self.state.transient_streak,
                last_probe_at: self.last_probe_at.map(|stamp| stamp.to_string()),
                last_failure: self.state.last_failure.clone(),
            },
            updated_at: now_millis(),
        };
        let Ok(mut text) = sonic_rs::to_string(&file) else {
            return;
        };
        text.push('\n');
        let _ = self
            .services
            .sidecar(
                &self.caller,
                SidecarOp::Write {
                    name,
                    bytes: text.into_bytes(),
                },
            )
            .await;
    }
}

/// Why the judge shared context could not be built; the batch stays
/// unreflected for every cause.
enum ContextError {
    /// The carried summaries, header, and oldest letter cannot fit the judge
    /// budget; `len` is the byte length the context would have needed.
    /// Non-retryable.
    TooLarge { len: usize },
    /// A selected letter resolves to no stored text. Non-retryable.
    SourceMissing,
    /// The host reported a failed read of a selected letter's text. The
    /// scheme surface folds resolver and store failures into one text, so
    /// this is non-retryable; the six-hour probe retries it. Non-retryable.
    SourceUnreadable,
    /// The text read was cancelled; reported, never counted as failure.
    Cancelled,
    /// Any other service failure; retried under the transient policy.
    Transient,
}

/// Returns the current UTC time as RFC 3339 with milliseconds.
fn now_millis() -> String {
    Timestamp::now()
        .round(Unit::Millisecond)
        .map_or_else(|_| Timestamp::now().to_string(), |stamp| stamp.to_string())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex, PoisonError};

    use dal_agent::error::ServiceError;
    use dal_agent::ext::services::ServiceFuture;
    use dal_agent::ext::{Caller, Doc, EventStream, RawValue, Services, Tool, ToolCx, ToolOutcome};
    use dal_core::ext::{
        McpDeclaration, McpRequest, McpResponse, StateError, StateOp, StateRecord, Visibility,
    };
    use dal_core::{
        AgentsOp, AgentsReply, Answer, DenyReason, EntryId, FetchRequest, FetchResponse, Inference,
        JobId, JobsOp, JobsReply, ModelRequest, ModelRoute, Notice, Question, RunOutput,
        RunRequest, SessionId, SidecarOp, TurnOp, TurnOpReply,
    };
    use dal_ext::judge::JudgeConfig;
    use tokio::sync::mpsc;

    use super::super::{Task, TaskMsg};
    use super::EPHEMERAL_NOTICE;

    fn locked<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        mutex.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records exactly what an ephemeral host would refuse and what the
    /// dream task must never touch: no job spawn, no sidecar write, no
    /// dream record; only the one exact unavailable notice.
    #[derive(Default)]
    struct EphemeralStub {
        notices: Mutex<Vec<String>>,
        jobs_spawned: Mutex<u32>,
        sidecar_writes: Mutex<u32>,
        records_appended: Mutex<u32>,
    }

    fn refused<T: Send + 'static>() -> ServiceFuture<'static, T> {
        Box::pin(async {
            Err(ServiceError::failed(
                None,
                "unavailable in the ephemeral dream stub",
            ))
        })
    }

    impl Services for EphemeralStub {
        fn fs_read(&self, _who: &Caller, _path: &str) -> ServiceFuture<'_, Option<Vec<u8>>> {
            refused()
        }

        fn fs_write(&self, _who: &Caller, _path: &str, _bytes: Vec<u8>) -> ServiceFuture<'_, ()> {
            refused()
        }

        fn net(&self, _who: &Caller, _req: FetchRequest) -> ServiceFuture<'_, FetchResponse> {
            refused()
        }

        fn run(&self, _who: &Caller, _req: RunRequest) -> ServiceFuture<'_, RunOutput> {
            refused()
        }

        fn env(&self, _who: &Caller, _key: &str) -> ServiceFuture<'_, Option<String>> {
            refused()
        }

        fn ask(&self, _who: &Caller, _question: Question) -> ServiceFuture<'_, Option<Answer>> {
            refused()
        }

        fn mcp(&self, _who: &Caller, _req: McpRequest) -> ServiceFuture<'_, McpResponse> {
            refused()
        }

        fn mcp_declarations(&self, _who: &Caller) -> ServiceFuture<'_, Vec<McpDeclaration>> {
            refused()
        }

        fn agents(&self, _who: &Caller, _op: AgentsOp) -> ServiceFuture<'_, AgentsReply> {
            refused()
        }

        fn jobs(&self, _who: &Caller, op: JobsOp) -> ServiceFuture<'_, JobsReply> {
            if matches!(op, JobsOp::Spawn { .. }) {
                *locked(&self.jobs_spawned) += 1;
                return Box::pin(async {
                    Ok(JobsReply::Spawned {
                        id: JobId::new_v7(),
                    })
                });
            }
            refused()
        }

        fn turn(&self, _who: &Caller, _op: TurnOp) -> ServiceFuture<'_, TurnOpReply> {
            refused()
        }

        fn history_texts(&self, _who: &Caller) -> ServiceFuture<'_, Vec<String>> {
            refused()
        }

        fn private_outcomes(
            &self,
            _who: &Caller,
        ) -> ServiceFuture<'_, Vec<dal_core::PrivateEffect>> {
            refused()
        }

        fn add_session_tools(
            &self,
            _who: &Caller,
            _tools: Vec<(Arc<dyn Tool>, Visibility)>,
        ) -> ServiceFuture<'_, ()> {
            refused()
        }

        fn open_asks(&self, _who: &Caller) -> ServiceFuture<'_, usize> {
            refused()
        }

        fn scheme(&self, _who: &Caller, _uri: &str) -> ServiceFuture<'_, Option<Doc>> {
            refused()
        }

        fn blob_put(&self, _who: &Caller, _bytes: Vec<u8>) -> ServiceFuture<'_, [u8; 32]> {
            refused()
        }

        fn blob_get(&self, _who: &Caller, _digest: [u8; 32]) -> ServiceFuture<'_, Option<Vec<u8>>> {
            refused()
        }

        fn state(
            &self,
            _who: &Caller,
            _op: StateOp,
        ) -> ServiceFuture<'_, Result<StateRecord, StateError>> {
            refused()
        }

        fn sidecar(&self, _who: &Caller, op: SidecarOp) -> ServiceFuture<'_, Option<Vec<u8>>> {
            match op {
                SidecarOp::Read { .. } => Box::pin(async {
                    Err(ServiceError::Denied(DenyReason::Unavailable {
                        what: "sidecar (ephemeral session)".into(),
                    }))
                }),
                SidecarOp::Write { .. } => {
                    *locked(&self.sidecar_writes) += 1;
                    Box::pin(async { Ok(None) })
                }
                _ => refused(),
            }
        }

        fn model_ready(
            &self,
            _who: &Caller,
            _model: ModelRoute,
        ) -> ServiceFuture<'_, Option<ModelRoute>> {
            refused()
        }

        fn infer(&self, _who: &Caller, _req: ModelRequest) -> ServiceFuture<'_, Inference> {
            refused()
        }

        fn infer_stream(
            &self,
            _who: &Caller,
            _req: ModelRequest,
        ) -> ServiceFuture<'_, EventStream> {
            refused()
        }

        fn call_tool(
            &self,
            _who: &Caller,
            _name: &str,
            _args: Box<RawValue>,
        ) -> ServiceFuture<'_, ToolOutcome> {
            refused()
        }

        fn notify(&self, _who: &Caller, notice: Notice) {
            locked(&self.notices).push(notice.text.to_string());
        }

        fn append_record(
            &self,
            _who: &Caller,
            _kind: &str,
            _body: Box<RawValue>,
        ) -> ServiceFuture<'_, EntryId> {
            *locked(&self.records_appended) += 1;
            Box::pin(async { Ok(EntryId::new(std::num::NonZeroU64::MIN)) })
        }

        fn records(&self, _who: &Caller, _kind: &str) -> ServiceFuture<'_, Vec<Box<RawValue>>> {
            Box::pin(async { Ok(Vec::new()) })
        }
    }

    #[tokio::test]
    async fn ephemeral_session_sends_the_exact_notice_without_a_job_or_sidecar_write() {
        let stub = Arc::new(EphemeralStub::default());
        let services: Arc<dyn Services> = stub.clone();
        let cx = ToolCx::for_test(Arc::clone(&services));
        let mut task = Task::new(
            SessionId::new_v7(),
            JudgeConfig::default(),
            None,
            "test-model".into(),
            services,
            cx.caller().clone(),
        );
        task.startup().await;
        assert!(
            task.unavailable,
            "an ephemeral sidecar denial marks the session unavailable"
        );
        let (_held, mut rx) = mpsc::channel::<TaskMsg>(8);
        task.submit(&mut rx).await;
        assert_eq!(
            locked(&stub.notices).as_slice(),
            ["history: auto-dream is unavailable in an ephemeral session."],
            "an ephemeral trigger sends exactly one notice with the exact text"
        );
        assert_eq!(
            EPHEMERAL_NOTICE,
            "history: auto-dream is unavailable in an ephemeral session."
        );
        assert_eq!(
            *locked(&stub.jobs_spawned),
            0,
            "an ephemeral trigger spawns no dream job"
        );
        assert_eq!(
            *locked(&stub.sidecar_writes),
            0,
            "an ephemeral trigger writes no sidecar"
        );
        assert_eq!(
            *locked(&stub.records_appended),
            0,
            "an ephemeral trigger commits no dream record"
        );
    }
}
