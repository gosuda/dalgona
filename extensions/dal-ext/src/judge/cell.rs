//! The cell-facing judge transport: inject and cell gating over one session judge.

use dal_agent::error::ServiceError;
use dal_agent::ext::services::{JudgeTransport, ServiceFuture};
use dal_core::Service;
use dal_core::ext::JudgeAuth;

use super::Judge;
use super::types::{JudgeError, JudgeQuestion, Verdict};

/// The session judge behind the cell `judge` entries.
///
/// The gate runs before any model work: a caller without the inject or
/// outside an eval cell sees [`JudgeError::Denied`], and an `Off` gate names
/// itself without opening a provider stream.
pub struct CellJudge {
    judge: Judge,
}

impl CellJudge {
    /// Wraps the session judge for the cell entries.
    #[must_use]
    pub fn new(judge: Judge) -> Self {
        Self { judge }
    }

    fn denied() -> ServiceError {
        ServiceError::failed(Some(Service::Judge), JudgeError::Denied.to_string())
    }

    fn unavailable() -> ServiceError {
        ServiceError::failed(
            Some(Service::Judge),
            "judge is unavailable (gate = off, judge = none)",
        )
    }

    fn failure(error: JudgeError) -> ServiceError {
        match error {
            JudgeError::Denied => Self::denied(),
            JudgeError::Unavailable => Self::unavailable(),
            other => ServiceError::failed(Some(Service::Judge), other.to_string()),
        }
    }

    fn answer(verdict: Verdict) -> Result<bool, ServiceError> {
        match verdict {
            Verdict::Bool(answer) => Ok(answer),
            _ => Err(ServiceError::failed(
                Some(Service::Judge),
                "judge: unexpected non-boolean verdict",
            )),
        }
    }
}

impl JudgeTransport for CellJudge {
    fn ask(&self, auth: JudgeAuth, question: String) -> ServiceFuture<'_, bool> {
        let judge = self.judge.clone();
        Box::pin(async move {
            if !auth.injected || !auth.cell {
                return Err(Self::denied());
            }
            let question = JudgeQuestion::bool(&question).map_err(Self::failure)?;
            let verdict = judge
                .judge("cell", "", question, auth.turn, None)
                .await
                .map_err(Self::failure)?;
            Self::answer(verdict)
        })
    }

    fn ask_batch(&self, auth: JudgeAuth, questions: Vec<String>) -> ServiceFuture<'_, Vec<bool>> {
        let judge = self.judge.clone();
        Box::pin(async move {
            if !auth.injected || !auth.cell {
                return Err(Self::denied());
            }
            let mut typed = Vec::with_capacity(questions.len());
            for question in &questions {
                typed.push(JudgeQuestion::bool(question).map_err(Self::failure)?);
            }
            let verdicts = judge
                .judge_batch("cell", "", typed, auth.turn, None)
                .await
                .map_err(Self::failure)?;
            verdicts.into_iter().map(Self::answer).collect()
        })
    }
}
