use thiserror::Error;

/// The result of resolving the session's judge role.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Gate {
    /// The judge role is ready and identifies its resolved model.
    Ready {
        /// The resolved model id serving judged completions.
        model_id: Box<str>,
    },
    /// The judge role is disabled or unavailable under the `auto` gate.
    Off,
}

/// A typed question the judge can answer.
///
/// Constructors validate framing limits. Public enum variants remain useful
/// to pattern-match answers, so request admission validates values again.
#[derive(Clone, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum JudgeQuestion {
    /// Ask for a boolean answer.
    Bool {
        /// One-line question text.
        prompt: Box<str>,
    },
    /// Ask the judge to select one option by its zero-based index.
    Choice {
        /// One-line question text.
        prompt: Box<str>,
        /// Ordered answer options.
        options: Box<[Box<str>]>,
    },
    /// Ask the judge to return an integer from zero through `max`, inclusive.
    Score {
        /// One-line question text.
        prompt: Box<str>,
        /// Inclusive upper bound, from 1 through 255.
        max: u8,
    },
}

/// A typed answer returned by the judge.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum Verdict {
    /// A boolean answer.
    Bool(bool),
    /// A zero-based choice index.
    Choice(u8),
    /// A score from zero through the question's inclusive maximum.
    Score(u8),
}

/// A judge operation failed before it could return a typed answer.
#[derive(Clone, Debug, Error)]
#[non_exhaustive]
pub enum JudgeError {
    /// The calling evaluation cell did not declare the judge service.
    #[error("judge: denied (declare \"judge\" in the cell's inject)")]
    Denied,
    /// The resolved session gate is off.
    #[error("judge unavailable")]
    Unavailable,
    /// A question did not satisfy the constructor or batch limits.
    #[error("invalid judge question: {reason}")]
    InvalidQuestion {
        /// A concise reason suitable for an error message.
        reason: Box<str>,
    },
    /// Shared context exceeds the UTF-8 byte limit.
    #[error("judge shared too large: {len} bytes (max 16384)")]
    SharedTooLarge {
        /// The rejected context size in bytes.
        len: usize,
    },
    /// The per-turn judge-call budget is exhausted.
    #[error("judge budget exhausted (max_per_turn = {max_per_turn})")]
    BudgetExhausted {
        /// The configured maximum calls in one turn window.
        max_per_turn: u32,
    },
    /// A model call or admission wait exceeded its deadline.
    #[error("judge timed out after {ms} ms")]
    Timeout {
        /// The effective timeout in milliseconds.
        ms: u64,
    },
    /// The model reply did not match the requested answer shape.
    #[error("judge parse error: {detail}")]
    Parse {
        /// A bounded, control-escaped prefix of the reply body.
        detail: Box<str>,
    },
    /// The inference service rejected or failed the request.
    #[error("judge provider error: {message}")]
    Provider {
        /// A bounded, control-escaped service error.
        message: Box<str>,
    },
}

impl JudgeQuestion {
    /// Constructs a boolean question.
    ///
    /// # Errors
    /// Returns [`JudgeError::InvalidQuestion`] when the prompt is empty,
    /// exceeds [`super::PROMPT_MAX`] bytes, or contains CR or LF.
    pub fn bool(prompt: &str) -> Result<Self, JudgeError> {
        validate_prompt(prompt)?;
        Ok(Self::Bool {
            prompt: prompt.into(),
        })
    }

    /// Constructs a multiple-choice question with zero-based answer indices.
    ///
    /// # Errors
    /// Returns [`JudgeError::InvalidQuestion`] when the prompt or options
    /// violate their byte, line, or count limits.
    pub fn choice(prompt: &str, options: &[&str]) -> Result<Self, JudgeError> {
        validate_prompt(prompt)?;
        if !(2..=26).contains(&options.len()) {
            return Err(invalid_question("a choice carries 2 to 26 options"));
        }
        let mut owned = Vec::with_capacity(options.len());
        for (index, option) in options.iter().enumerate() {
            validate_option(index + 1, option)?;
            owned.push(Box::<str>::from(*option));
        }
        Ok(Self::Choice {
            prompt: prompt.into(),
            options: owned.into_boxed_slice(),
        })
    }

    /// Constructs a score question with an inclusive maximum.
    ///
    /// # Errors
    /// Returns [`JudgeError::InvalidQuestion`] when the prompt is invalid or
    /// `max` is zero.
    pub fn score(prompt: &str, max: u8) -> Result<Self, JudgeError> {
        validate_prompt(prompt)?;
        if max == 0 {
            return Err(invalid_question("score max must be 1 to 255"));
        }
        Ok(Self::Score {
            prompt: prompt.into(),
            max,
        })
    }

    pub(super) fn validate(&self) -> Result<(), JudgeError> {
        match self {
            Self::Bool { prompt } => validate_prompt(prompt),
            Self::Choice { prompt, options } => {
                validate_prompt(prompt)?;
                if !(2..=26).contains(&options.len()) {
                    return Err(invalid_question("a choice carries 2 to 26 options"));
                }
                for (index, option) in options.iter().enumerate() {
                    validate_option(index + 1, option)?;
                }
                Ok(())
            }
            Self::Score { prompt, max } => {
                validate_prompt(prompt)?;
                if *max == 0 {
                    return Err(invalid_question("score max must be 1 to 255"));
                }
                Ok(())
            }
        }
    }
}

fn validate_prompt(prompt: &str) -> Result<(), JudgeError> {
    if prompt.is_empty() {
        return Err(invalid_question("prompt must not be empty"));
    }
    if prompt.len() > super::PROMPT_MAX {
        return Err(invalid_question(format!(
            "prompt is {} bytes (max {})",
            prompt.len(),
            super::PROMPT_MAX
        )));
    }
    if prompt.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err(invalid_question("prompt must be one line"));
    }
    Ok(())
}

fn validate_option(index: usize, option: &str) -> Result<(), JudgeError> {
    if option.is_empty() {
        return Err(invalid_question(format!(
            "option {index} must not be empty"
        )));
    }
    if option.len() > super::OPTION_MAX {
        return Err(invalid_question(format!(
            "option {index} is {} bytes (max {})",
            option.len(),
            super::OPTION_MAX
        )));
    }
    if option.bytes().any(|byte| matches!(byte, b'\r' | b'\n')) {
        return Err(invalid_question(format!("option {index} must be one line")));
    }
    Ok(())
}

fn invalid_question(reason: impl Into<Box<str>>) -> JudgeError {
    JudgeError::InvalidQuestion {
        reason: reason.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::{JudgeError, JudgeQuestion};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn prompt_byte_limit_accepts_boundary_and_rejects_next_byte() -> TestResult {
        let accepted = "a".repeat(4096);
        assert!(JudgeQuestion::bool(&accepted).is_ok());
        let rejected = "a".repeat(4097);
        assert!(matches!(
            JudgeQuestion::bool(&rejected),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "prompt is 4097 bytes (max 4096)"
        ));
        Ok(())
    }

    #[test]
    fn prompt_limit_counts_utf8_bytes() -> TestResult {
        let accepted = "é".repeat(2048);
        assert!(JudgeQuestion::bool(&accepted).is_ok());
        let rejected = "é".repeat(2049);
        assert!(matches!(
            JudgeQuestion::bool(&rejected),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "prompt is 4098 bytes (max 4096)"
        ));
        Ok(())
    }

    #[test]
    fn bool_question_rejects_empty_or_multiline_prompt() {
        assert!(matches!(
            JudgeQuestion::bool(""),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "prompt must not be empty"
        ));
        assert!(matches!(
            JudgeQuestion::bool("one\ntwo"),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "prompt must be one line"
        ));
    }

    #[test]
    fn choice_question_checks_option_count_and_byte_boundary() -> TestResult {
        let two = ["keep", "revert"];
        assert!(JudgeQuestion::choice("choose", &two).is_ok());
        assert!(matches!(
            JudgeQuestion::choice("choose", &["only"]),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "a choice carries 2 to 26 options"
        ));
        let options = ["a"; 27];
        assert!(matches!(
            JudgeQuestion::choice("choose", &options),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "a choice carries 2 to 26 options"
        ));
        let accepted = "a".repeat(200);
        assert!(JudgeQuestion::choice("choose", &[&accepted, "no"]).is_ok());
        let rejected = "a".repeat(201);
        assert!(matches!(
            JudgeQuestion::choice("choose", &[&rejected, "no"]),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "option 1 is 201 bytes (max 200)"
        ));
        Ok(())
    }

    #[test]
    fn choice_question_rejects_empty_or_multiline_option() {
        assert!(matches!(
            JudgeQuestion::choice("choose", &["", "other"]),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "option 1 must not be empty"
        ));
        assert!(matches!(
            JudgeQuestion::choice("choose", &["one\ntwo", "other"]),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "option 1 must be one line"
        ));
    }

    #[test]
    fn score_question_requires_nonzero_inclusive_maximum() -> TestResult {
        assert!(JudgeQuestion::score("rate", u8::MAX).is_ok());
        assert!(matches!(
            JudgeQuestion::score("rate", 0),
            Err(JudgeError::InvalidQuestion { reason })
                if reason.as_ref() == "score max must be 1 to 255"
        ));
        Ok(())
    }
}
