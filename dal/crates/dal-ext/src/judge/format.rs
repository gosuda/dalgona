use std::fmt::Write as _;

use super::{JudgeError, JudgeQuestion, Verdict};
use serde::Deserialize;
use sonic_rs::{JsonValueTrait, Value};

/// The system message used for typed judge requests.
pub const SYSTEM_LINE: &str =
    "You are a judge. Reply with one line of JSON only. No prose, no code fences.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerEnvelope {
    answers: Vec<Value>,
}

/// Renders the fixed judge envelope for a batch of questions.
///
/// The caller validates question counts and values before rendering.
#[must_use]
pub fn render_envelope(shared: &str, questions: &[JudgeQuestion]) -> String {
    let mut rendered = String::with_capacity(shared.len() + 256);
    rendered.push_str("Shared context:\n<<<SHARED\n");
    rendered.push_str(shared);
    rendered.push_str("\nSHARED>>>\n\nQuestions:\n");

    for (index, question) in questions.iter().enumerate() {
        if index != 0 {
            rendered.push('\n');
        }
        render_question(&mut rendered, index + 1, question);
    }

    rendered.push_str("\n\nRespond with exactly: {\"answers\": [");
    for index in 1..=questions.len() {
        if index != 1 {
            rendered.push_str(", ");
        }
        let _ = write!(rendered, "<answer {index}>");
    }
    rendered.push_str("]}");
    rendered
}

/// A text-only system message for the history-summary operation used by
/// dalgona history consolidation.
pub(super) const SUMMARY_SYSTEM_LINE: &str =
    "You are a judge. Follow the summary request and reply with plain text only.";

/// Renders the text-only summary request used by dalgona history consolidation.
#[must_use]
pub(super) fn render_summary(shared: &str, prompt: &str) -> String {
    let mut rendered = String::with_capacity(shared.len() + prompt.len() + 128);
    rendered.push_str("Shared context:\n<<<SHARED\n");
    rendered.push_str(shared);
    rendered.push_str("\nSHARED>>>\n\nSummary request:\n");
    rendered.push_str(prompt);
    rendered
}

/// Parses exactly one typed answer per question from a JSON object.
///
/// # Errors
/// Returns [`JudgeError::Parse`] unless the body has exactly one `answers`
/// array with the required length and an answer value in range for every
/// question.
pub fn parse_answers(body: &str, questions: &[JudgeQuestion]) -> Result<Vec<Verdict>, JudgeError> {
    let parsed: AnswerEnvelope = sonic_rs::from_str(body.trim()).map_err(|_| parse_error(body))?;
    if parsed.answers.len() != questions.len() {
        return Err(parse_error(body));
    }

    let mut verdicts = Vec::with_capacity(questions.len());
    for (answer, question) in parsed.answers.iter().zip(questions) {
        let verdict = match question {
            JudgeQuestion::Bool { .. } => answer
                .as_bool()
                .map(Verdict::Bool)
                .ok_or_else(|| parse_error(body))?,
            JudgeQuestion::Choice { options, .. } => {
                let Some(index) = answer.as_u64() else {
                    return Err(parse_error(body));
                };
                let Ok(option_count) = u64::try_from(options.len()) else {
                    return Err(parse_error(body));
                };
                if index >= option_count {
                    return Err(parse_error(body));
                }
                let Ok(index) = u8::try_from(index) else {
                    return Err(parse_error(body));
                };
                Verdict::Choice(index)
            }
            JudgeQuestion::Score { max, .. } => {
                let Some(score) = answer.as_u64() else {
                    return Err(parse_error(body));
                };
                if score > u64::from(*max) {
                    return Err(parse_error(body));
                }
                let Ok(score) = u8::try_from(score) else {
                    return Err(parse_error(body));
                };
                Verdict::Score(score)
            }
        };
        verdicts.push(verdict);
    }
    Ok(verdicts)
}

pub(super) fn reply_detail(body: &str) -> Box<str> {
    let mut end = body.len().min(200);
    while !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut detail = String::with_capacity(end);
    for character in body[..end].chars() {
        if character.is_control() {
            let _ = write!(detail, "\\u{:04x}", u32::from(character));
        } else {
            detail.push(character);
        }
    }
    detail.into_boxed_str()
}

fn parse_error(body: &str) -> JudgeError {
    JudgeError::Parse {
        detail: reply_detail(body),
    }
}

fn render_question(rendered: &mut String, number: usize, question: &JudgeQuestion) {
    match question {
        JudgeQuestion::Bool { prompt } => {
            let _ = write!(
                rendered,
                "{number}. {prompt}\n   kind: bool. Answer true or false."
            );
        }
        JudgeQuestion::Choice { prompt, options } => {
            let _ = write!(
                rendered,
                "{number}. {prompt}\n   kind: choice. Answer one integer index. "
            );
            for (index, option) in options.iter().enumerate() {
                if index != 0 {
                    rendered.push_str(", ");
                }
                let _ = write!(rendered, "{index} = \"{option}\"");
            }
            rendered.push('.');
        }
        JudgeQuestion::Score { prompt, max } => {
            let _ = write!(
                rendered,
                "{number}. {prompt}\n   kind: score. Answer one integer in 0..{max}."
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{
        SUMMARY_SYSTEM_LINE, SYSTEM_LINE, parse_answers, render_envelope, render_summary,
        reply_detail,
    };
    use crate::judge::{JudgeQuestion, Verdict};

    type TestResult = Result<(), Box<dyn std::error::Error>>;

    #[test]
    fn typed_answers_preserve_question_order_and_kind() -> TestResult {
        let questions = [
            JudgeQuestion::bool("safe?")?,
            JudgeQuestion::choice("action?", &["keep", "revert", "ask"])?,
            JudgeQuestion::score("quality", 5)?,
        ];
        let answers = parse_answers(r#"{"answers":[false,2,4]}"#, &questions)?;
        assert_eq!(
            answers,
            [Verdict::Bool(false), Verdict::Choice(2), Verdict::Score(4)]
        );
        Ok(())
    }

    #[test]
    fn envelope_has_fixed_framing_and_one_answer_slot_per_question() -> TestResult {
        let questions = [
            JudgeQuestion::choice("choose", &["keep", "revert", "ask"])?,
            JudgeQuestion::score("rate", 5)?,
        ];
        let rendered = render_envelope("work in progress", &questions);
        assert_eq!(
            rendered,
            concat!(
                "Shared context:\n<<<SHARED\nwork in progress\nSHARED>>>\n\nQuestions:\n",
                "1. choose\n   kind: choice. Answer one integer index. 0 = \"keep\", ",
                "1 = \"revert\", 2 = \"ask\".\n",
                "2. rate\n   kind: score. Answer one integer in 0..5.\n\n",
                "Respond with exactly: {\"answers\": [<answer 1>, <answer 2>]}"
            )
        );
        assert_eq!(
            SYSTEM_LINE,
            "You are a judge. Reply with one line of JSON only. No prose, no code fences."
        );
        Ok(())
    }

    #[test]
    fn parser_rejects_non_object_extra_members_and_duplicate_answer_keys() -> TestResult {
        let question = [JudgeQuestion::bool("continue?")?];
        for body in [
            r#"true"#,
            r#"{}"#,
            r#"{"answer":[true]}"#,
            r#"{"answers":[true, false]}"#,
            r#"{"answers":[true],"why":"x"}"#,
            r#"{"answers":[true],"answers":[false]}"#,
        ] {
            assert!(parse_answers(body, &question).is_err(), "accepted {body}");
        }
        Ok(())
    }

    #[test]
    fn parser_rejects_wrong_types_fractional_exponent_and_out_of_range_values() -> TestResult {
        let bool_question = [JudgeQuestion::bool("continue?")?];
        let choice_question = [JudgeQuestion::choice("choose", &["keep", "revert", "ask"])?];
        let score_question = [JudgeQuestion::score("rate", 5)?];
        for body in [
            r#"{"answers":["true"]}"#,
            r#"{"answers":[null]}"#,
            r#"{"answers":[1]}"#,
        ] {
            assert!(
                parse_answers(body, &bool_question).is_err(),
                "accepted {body}"
            );
        }
        for body in [r#"{"answers":[1.0]}"#, r#"{"answers":[1e0]}"#] {
            assert!(
                parse_answers(body, &choice_question).is_err(),
                "accepted {body}"
            );
        }
        assert!(parse_answers(r#"{"answers":[3]}"#, &choice_question).is_err());
        assert!(parse_answers(r#"{"answers":[6]}"#, &score_question).is_err());
        assert_eq!(
            parse_answers(r#"{"answers":[5]}"#, &score_question)?,
            [Verdict::Score(5)]
        );
        Ok(())
    }

    #[test]
    fn parser_error_detail_is_raw_prefix_with_control_escapes() {
        assert_eq!(reply_detail("").as_ref(), "");
        assert_eq!(reply_detail("bad\nreply").as_ref(), "bad\\u000areply");
        let body = format!("{}é", "x".repeat(199));
        let detail = reply_detail(&body);
        assert_eq!(detail.len(), 199);
        assert!(detail.is_char_boundary(detail.len()));
    }

    #[test]
    fn summary_request_renders_shared_verbatim_with_plain_text_system() {
        let rendered = render_summary("work in progress", "Condense the open items.");
        assert_eq!(
            rendered,
            "Shared context:\n<<<SHARED\nwork in progress\nSHARED>>>\n\nSummary request:\nCondense the open items."
        );
        assert_eq!(
            SUMMARY_SYSTEM_LINE,
            "You are a judge. Follow the summary request and reply with plain text only."
        );
    }
}
