// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Review session records, round transitions, and finding identities.

use std::collections::BTreeSet;
use std::fmt::Write as _;

use dal_core::SessionId;
use serde::{Deserialize, Serialize};

use super::reply::{ReviewerReply, Severity, Verdict, escape_report_text};
use super::{MAX_STORED_DETAIL_BYTES, ReviewError, utf8_prefix};

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct ReviewRecord {
    pub(crate) session: SessionId,
    pub(crate) round: u8,
    pub(crate) verdict: Verdict,
    pub(crate) new_count: u8,
    pub(crate) findings: Vec<StoredFinding>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ReviewRound {
    pub(crate) session: SessionId,
    pub(crate) round: u8,
}

impl ReviewRound {
    pub(crate) fn new_session() -> Self {
        Self {
            session: SessionId::new_v7(),
            round: 1,
        }
    }
}

/// What the caller asked for when it opens a review round.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum RoundRequest {
    /// Continue the open session; stop at the cap.
    Continue,
    /// Start a new session when none is open or the open one reached its cap.
    Restart,
}

/// Picks the next round of the open review session.
///
/// A converged or clean session starts a new one. A session that used all its
/// rounds without converging stops with the findings still open; only
/// [`RoundRequest::Restart`] goes past that cap. A restart request never
/// discards rounds that are still in progress.
pub(crate) fn next_round(
    records: &[ReviewRecord],
    max_rounds: u8,
    request: RoundRequest,
) -> Result<ReviewRound, ReviewError> {
    let Some(last) = records.last() else {
        return Ok(ReviewRound::new_session());
    };
    if last.verdict == Verdict::Clean || last.new_count == 0 {
        return Ok(ReviewRound::new_session());
    }
    if last.round >= max_rounds {
        return match request {
            RoundRequest::Restart => Ok(ReviewRound::new_session()),
            RoundRequest::Continue => Err(ReviewError::CapReached {
                rounds: max_rounds,
                outstanding: outstanding_findings(&last.findings),
            }),
        };
    }
    Ok(ReviewRound {
        session: last.session,
        round: last.round.saturating_add(1),
    })
}

/// Lists findings for the user: severity, location, and title, one per line.
pub(crate) fn outstanding_findings(findings: &[StoredFinding]) -> Box<str> {
    let mut listing = String::new();
    for finding in findings {
        if !listing.is_empty() {
            listing.push('\n');
        }
        let _ = write!(
            listing,
            "- [{}] {}",
            finding.severity.as_str(),
            escape_report_text(&finding.path),
        );
        if let Some(line) = finding.line {
            let _ = write!(listing, ":{line}");
        }
        let _ = write!(listing, " {}", escape_report_text(finding.title.trim()));
    }
    listing.into_boxed_str()
}

#[derive(Debug, Deserialize, Serialize)]
pub(crate) struct StoredFinding {
    pub(crate) path: String,
    pub(crate) line: Option<i64>,
    pub(crate) severity: Severity,
    pub(crate) title: String,
    pub(crate) detail: String,
}

#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) struct FindingIdentity {
    pub(crate) path: String,
    pub(crate) title: String,
}
pub(crate) fn normalize_title(title: &str) -> String {
    let mut normalized = String::with_capacity(title.len());
    let mut whitespace = false;
    for character in title.trim().chars() {
        if character.is_whitespace() {
            if !normalized.is_empty() && !whitespace {
                normalized.push(' ');
            }
            whitespace = true;
        } else {
            normalized.extend(character.to_lowercase());
            whitespace = false;
        }
    }
    normalized
}

pub(crate) fn identity(path: &str, title: &str) -> FindingIdentity {
    FindingIdentity {
        path: path.to_owned(),
        title: normalize_title(title),
    }
}
pub(crate) fn stored_findings(reply: &ReviewerReply) -> Vec<StoredFinding> {
    reply
        .findings
        .iter()
        .map(|finding| StoredFinding {
            path: finding.path.clone(),
            line: finding.line.0,
            severity: finding.severity,
            title: finding.title.clone(),
            detail: utf8_prefix(&finding.detail, MAX_STORED_DETAIL_BYTES).to_owned(),
        })
        .collect()
}

pub(crate) fn earlier_identities(
    records: &[ReviewRecord],
    session: SessionId,
) -> BTreeSet<FindingIdentity> {
    records
        .iter()
        .filter(|record| record.session == session)
        .flat_map(|record| {
            record
                .findings
                .iter()
                .map(|finding| identity(&finding.path, &finding.title))
        })
        .collect()
}

pub(crate) fn new_count(
    reply: &ReviewerReply,
    earlier: &BTreeSet<FindingIdentity>,
) -> Result<u8, ReviewError> {
    let count = reply
        .findings
        .iter()
        .filter(|finding| !earlier.contains(&identity(&finding.path, &finding.title)))
        .count();
    u8::try_from(count).map_err(|_| ReviewError::TooManyFindings { count })
}

pub(crate) fn prior_findings(records: &[ReviewRecord], session: SessionId) -> String {
    let identities = earlier_identities(records, session);
    if identities.is_empty() {
        return "none".to_owned();
    }
    let mut text = String::new();
    for identity in identities {
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&identity.path);
        text.push('\t');
        text.push_str(&identity.title);
    }
    text
}
