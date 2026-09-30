use dal_core::{RawJson, RawJsonError, TurnId};
use serde::Serialize;
use std::fmt::Write as _;

use super::JudgeError;

const MAX_ROW_BYTES: usize = 1024;
const CAUSE_MAX_BYTES: usize = 200;
const FIELD_MAX_BYTES: usize = 100;

#[derive(Serialize)]
pub(super) struct JudgeRow {
    kind: &'static str,
    call: u64,
    turn: Option<TurnId>,
    feature: Box<str>,
    questions: usize,
    status: &'static str,
    cause: Box<str>,
    model: Box<str>,
    duration_ms: u64,
    slot_wait_ms: u64,
    input_tokens: u64,
    output_tokens: u64,
}

impl JudgeRow {
    pub(super) fn new(
        call: u64,
        turn: Option<TurnId>,
        feature: &str,
        questions: usize,
        status: &'static str,
        cause: &str,
        model: &str,
        duration_ms: u64,
        slot_wait_ms: u64,
        input_tokens: u64,
        output_tokens: u64,
    ) -> Self {
        Self {
            kind: "judge",
            call,
            turn,
            feature: bounded_text(feature, FIELD_MAX_BYTES, FIELD_MAX_BYTES),
            questions,
            status,
            cause: bounded_text(cause, CAUSE_MAX_BYTES, CAUSE_MAX_BYTES),
            model: bounded_text(model, FIELD_MAX_BYTES, FIELD_MAX_BYTES),
            duration_ms,
            slot_wait_ms,
            input_tokens,
            output_tokens,
        }
    }

    pub(super) fn into_raw(mut self) -> Result<RawJson, JudgeError> {
        loop {
            let serialized = sonic_rs::to_string(&self).map_err(|error| JudgeError::Provider {
                message: bounded_text(
                    &format!("judge ledger serialization failed: {error}"),
                    CAUSE_MAX_BYTES,
                    CAUSE_MAX_BYTES,
                ),
            })?;
            if serialized.len() <= MAX_ROW_BYTES {
                return RawJson::parse(&serialized).map_err(raw_error);
            }
            if self.cause.is_empty() {
                return Err(JudgeError::Provider {
                    message: Box::from("judge ledger row exceeds 1024 bytes"),
                });
            }
            let mut next_len = self.cause.len() - 1;
            while !self.cause.is_char_boundary(next_len) {
                next_len -= 1;
            }
            self.cause = self.cause[..next_len].into();
        }
    }
}

fn bounded_text(value: &str, input_limit: usize, output_limit: usize) -> Box<str> {
    let mut end = value.len().min(input_limit);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    let mut output = String::with_capacity(end.min(output_limit));
    for character in value[..end].chars() {
        if character.is_control() {
            let previous_len = output.len();
            let _ = write!(output, "\\u{:04x}", u32::from(character));
            if output.len() > output_limit {
                output.truncate(previous_len);
                break;
            }
            continue;
        }
        if output.len() + character.len_utf8() > output_limit {
            break;
        }
        output.push(character);
    }
    output.into_boxed_str()
}

fn raw_error(error: RawJsonError) -> JudgeError {
    JudgeError::Provider {
        message: bounded_text(&error.to_string(), CAUSE_MAX_BYTES, CAUSE_MAX_BYTES),
    }
}

#[cfg(test)]
mod tests {
    use super::JudgeRow;
    use sonic_rs::JsonValueTrait;

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn ledger_serializes_redacted_fields_in_contract_order() -> TestResult {
        let row = JudgeRow::new(
            42,
            None,
            "ttsr",
            2,
            "ok",
            "",
            "gpt-5.6-luna",
            812,
            3,
            1042,
            9,
        );
        let raw = row.into_raw()?;
        assert_eq!(
            raw.as_str(),
            r#"{"kind":"judge","call":42,"turn":null,"feature":"ttsr","questions":2,"status":"ok","cause":"","model":"gpt-5.6-luna","duration_ms":812,"slot_wait_ms":3,"input_tokens":1042,"output_tokens":9}"#
        );
        Ok(())
    }

    #[test]
    fn ledger_bounds_escaped_fields_and_stays_under_size_cap() -> TestResult {
        let row = JudgeRow::new(
            1,
            None,
            &"\0".repeat(1000),
            32,
            "provider",
            &"\0".repeat(1000),
            &"\0".repeat(1000),
            1,
            2,
            3,
            4,
        );
        let raw = row.into_raw()?;
        assert!(raw.as_str().len() <= 1024);
        let value: sonic_rs::Value = sonic_rs::from_str(raw.as_str())?;
        assert_eq!(
            value.get("kind").and_then(JsonValueTrait::as_str),
            Some("judge")
        );
        assert_eq!(value.get("call").and_then(JsonValueTrait::as_u64), Some(1));
        assert!(
            value
                .get("cause")
                .and_then(JsonValueTrait::as_str)
                .is_some()
        );
        Ok(())
    }
}
