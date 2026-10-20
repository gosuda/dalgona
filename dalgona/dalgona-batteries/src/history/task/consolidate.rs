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
