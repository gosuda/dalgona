// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Reviewer reply decoding, validation, and deterministic rendering.

use std::{collections::BTreeSet, fmt::Write as _};

use serde::{Deserialize, Deserializer, Serialize, de};

use super::rounds::{FindingIdentity, identity, outstanding_findings, stored_findings};
use super::{
    DIFF_TRUNCATION_MARKER, FOCUS_TRUNCATION_MARKER, MAX_ERROR_BYTES, MAX_FINDINGS,
    REVIEW_REPLY_FORMAT, ReviewError, utf8_prefix,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Verdict {
    Clean,
    Findings,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum Severity {
    Critical,
    Major,
    Minor,
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReviewerReply {
    #[serde(deserialize_with = "deserialize_verdict")]
    pub(crate) verdict: Verdict,
    #[serde(deserialize_with = "deserialize_findings")]
    pub(crate) findings: Vec<Finding>,
    #[serde(deserialize_with = "deserialize_summary")]
    pub(crate) summary: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Finding {
    #[serde(deserialize_with = "deserialize_path")]
    pub(crate) path: String,
    #[serde(deserialize_with = "deserialize_line")]
    pub(crate) line: RequiredLine,
    #[serde(deserialize_with = "deserialize_severity")]
    pub(crate) severity: Severity,
    #[serde(deserialize_with = "deserialize_title")]
    pub(crate) title: String,
    #[serde(deserialize_with = "deserialize_detail")]
    pub(crate) detail: String,
}

#[derive(Debug, Deserialize)]
#[serde(transparent)]
pub(crate) struct RequiredLine(pub(crate) Option<i64>);

pub(crate) fn deserialize_verdict<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Verdict, D::Error> {
    Verdict::deserialize(deserializer)
        .map_err(|error| de::Error::custom(format!("verdict: {error}")))
}

pub(crate) fn deserialize_findings<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Finding>, D::Error> {
    Vec::<Finding>::deserialize(deserializer)
        .map_err(|error| de::Error::custom(format!("findings: {error}")))
}

pub(crate) fn deserialize_summary<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    String::deserialize(deserializer)
        .map_err(|error| de::Error::custom(format!("summary: {error}")))
}

pub(crate) fn deserialize_path<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    String::deserialize(deserializer).map_err(|error| de::Error::custom(format!("path: {error}")))
}

pub(crate) fn deserialize_line<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<RequiredLine, D::Error> {
    RequiredLine::deserialize(deserializer)
        .map_err(|error| de::Error::custom(format!("line: {error}")))
}

pub(crate) fn deserialize_severity<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Severity, D::Error> {
    Severity::deserialize(deserializer)
        .map_err(|error| de::Error::custom(format!("severity: {error}")))
}

pub(crate) fn deserialize_title<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    String::deserialize(deserializer).map_err(|error| de::Error::custom(format!("title: {error}")))
}

pub(crate) fn deserialize_detail<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<String, D::Error> {
    String::deserialize(deserializer).map_err(|error| de::Error::custom(format!("detail: {error}")))
}

pub(crate) fn parse_reply(body: &str, round: u8) -> Result<ReviewerReply, ReviewError> {
    let reply = sonic_rs::from_str::<ReviewerReply>(body).map_err(|error| ReviewError::Parse {
        round,
        reason: cap_error(&error.to_string()),
    })?;
    validate_reply(reply, round)
}
pub(crate) fn enforce_findings_cap(reply: ReviewerReply) -> Result<ReviewerReply, ReviewError> {
    let count = reply.findings.len();
    if count > MAX_FINDINGS {
        return Err(ReviewError::TooManyFindings { count });
    }
    Ok(reply)
}

pub(crate) fn validate_reply(
    reply: ReviewerReply,
    round: u8,
) -> Result<ReviewerReply, ReviewError> {
    let invalid = if !(1..=500).contains(&reply.summary.len()) {
        Some("summary must be 1 to 500 UTF-8 bytes")
    } else if (reply.verdict == Verdict::Clean) != reply.findings.is_empty() {
        Some("findings must be empty exactly when verdict is clean")
    } else {
        reply.findings.iter().find_map(|finding| {
            if !(1..=1024).contains(&finding.path.len()) {
                Some("path must be 1 to 1024 UTF-8 bytes")
            } else if !(1..=200).contains(&finding.title.len()) {
                Some("title must be 1 to 200 UTF-8 bytes")
            } else if !(1..=2000).contains(&finding.detail.len()) {
                Some("detail must be 1 to 2000 UTF-8 bytes")
            } else {
                None
            }
        })
    };
    if let Some(reason) = invalid {
        return Err(ReviewError::Parse {
            round,
            reason: reason.into(),
        });
    }
    Ok(reply)
}

pub(crate) fn cap_error(value: &str) -> Box<str> {
    utf8_prefix(value, MAX_ERROR_BYTES).into()
}
pub(crate) fn append_prompt_section(prompt: &mut String, heading: &str, body: &str) {
    prompt.push_str(heading);
    prompt.push('\n');
    prompt.push_str(body);
    if !prompt.ends_with('\n') {
        prompt.push('\n');
    }
}

pub(crate) fn review_content(diff: &str, status: &str, focus: Option<&str>, prior: &str) -> String {
    let focus = focus.unwrap_or("none");
    let mut prompt = String::with_capacity(
        diff.len()
            + status.len()
            + focus.len()
            + prior.len()
            + REVIEW_REPLY_FORMAT.len()
            + "## Diff\n".len()
            + "## Status\n".len()
            + "## Focus\n".len()
            + "## Prior findings\n".len()
            + "## Reply format\n".len()
            + 4,
    );
    append_prompt_section(&mut prompt, "## Diff", diff);
    append_prompt_section(&mut prompt, "## Status", status);
    append_prompt_section(&mut prompt, "## Focus", focus);
    append_prompt_section(&mut prompt, "## Prior findings", prior);
    prompt.push_str("## Reply format\n");
    prompt.push_str(REVIEW_REPLY_FORMAT);
    prompt
}
pub(crate) fn quote_json_string(value: &str) -> String {
    let mut quoted = String::with_capacity(value.len() + 2);
    quoted.push('"');
    for character in value.chars() {
        match character {
            '"' => quoted.push_str("\\\""),
            '\\' => quoted.push_str("\\\\"),
            '\u{08}' => quoted.push_str("\\b"),
            '\u{0c}' => quoted.push_str("\\f"),
            '\n' => quoted.push_str("\\n"),
            '\r' => quoted.push_str("\\r"),
            '\t' => quoted.push_str("\\t"),
            character if character.is_control() => {
                let _ = write!(quoted, "\\u{:04x}", u32::from(character));
            }
            character => quoted.push(character),
        }
    }
    quoted.push('"');
    quoted
}

pub(crate) fn escape_report_text(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            '\0' => escaped.push_str("\\0"),
            '\u{2028}' => escaped.push_str("\\u{2028}"),
            '\u{2029}' => escaped.push_str("\\u{2029}"),
            character if character.is_control() => {
                let _ = write!(escaped, "\\u{{{:x}}}", u32::from(character));
            }
            character => escaped.push(character),
        }
    }
    escaped
}
pub(crate) fn render_report(
    round: u8,
    max_rounds: u8,
    reply: &ReviewerReply,
    earlier: &BTreeSet<FindingIdentity>,
    diff_truncated: bool,
) -> String {
    let mut output = format!(
        "Review round {round} of {max_rounds}: findings.\nSummary: {}",
        quote_json_string(&reply.summary)
    );
    if diff_truncated {
        output.push_str(DIFF_TRUNCATION_MARKER);
    }
    for finding in &reply.findings {
        let finding_identity = identity(&finding.path, &finding.title);
        let status = if earlier.contains(&finding_identity) {
            "repeat"
        } else {
            "new"
        };
        let line = finding
            .line
            .0
            .map_or_else(|| "?".to_owned(), |line| line.to_string());
        let _ = write!(
            output,
            "\n- [{}] {}:{} {} ({status})\n  {}",
            finding.severity.as_str(),
            escape_report_text(&finding.path),
            line,
            escape_report_text(&finding.title),
            escape_report_text(&finding.detail)
        );
    }
    output.push_str("\nFix the new findings and call review again.");
    output
}

pub(crate) fn settle_reply(
    round: u8,
    max_rounds: u8,
    reply: &ReviewerReply,
    earlier: &BTreeSet<FindingIdentity>,
    new_count: u8,
    diff_truncated: bool,
) -> Result<String, ReviewError> {
    if reply.verdict == Verdict::Clean {
        return Ok(format!(
            "Review round {round} of {round}: clean. No findings."
        ));
    }
    if new_count == 0 {
        return Ok(format!("Converged after {round} rounds: no new findings."));
    }
    if round >= max_rounds {
        return Err(ReviewError::CapReached {
            rounds: max_rounds,
            outstanding: outstanding_findings(&stored_findings(reply)),
        });
    }
    Ok(render_report(
        round,
        max_rounds,
        reply,
        earlier,
        diff_truncated,
    ))
}

impl Severity {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::Critical => "critical",
            Self::Major => "major",
            Self::Minor => "minor",
        }
    }
}
const COMMAND_PROMPT_BASE: &str = "Review the current changes with the review tool. The user ran /review, which grants one restart; call review with restart set to true.";

pub(crate) fn command_prompt(focus: &str) -> String {
    if focus.is_empty() {
        return COMMAND_PROMPT_BASE.to_owned();
    }
    let focus = if focus.len() > 500 {
        format!("{}{}", utf8_prefix(focus, 500), FOCUS_TRUNCATION_MARKER)
    } else {
        focus.to_owned()
    };
    format!("{COMMAND_PROMPT_BASE}\nFocus: {focus}")
}
