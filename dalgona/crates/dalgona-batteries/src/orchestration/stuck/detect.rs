// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Loop detectors run in order per call: identical, cycle, similar.

use std::collections::{HashMap, VecDeque};

use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::guard::{DetectorKind, ToolCallRecord};
use super::{
    CYCLE_NOTICE_HEAD, CYCLE_NOTICE_TAIL, GuardError, IDENTICAL_NOTICE, RECORD_CAPACITY,
    SIMILAR_NOTICE, render_template,
};

/// One admitted-or-candidate detection against the window ending now.
#[derive(Debug)]
pub(super) enum Detection {
    Identical {
        fingerprint: Box<str>,
        tool: Box<str>,
        count: u32,
    },
    Cycle {
        fingerprint: Box<str>,
        tools: Vec<Box<str>>,
        period: u32,
        count: u32,
    },
    Similar {
        fingerprint: Box<str>,
        tool: Box<str>,
        count: u32,
        similarity: f64,
    },
}

impl Detection {
    pub(super) fn kind(&self) -> DetectorKind {
        match self {
            Self::Identical { .. } => DetectorKind::Identical,
            Self::Cycle { .. } => DetectorKind::Cycle,
            Self::Similar { .. } => DetectorKind::Similar,
        }
    }

    pub(super) fn fingerprint(&self) -> &str {
        match self {
            Self::Identical { fingerprint, .. }
            | Self::Cycle { fingerprint, .. }
            | Self::Similar { fingerprint, .. } => fingerprint,
        }
    }

    pub(super) fn count(&self) -> u32 {
        match self {
            Self::Identical { count, .. }
            | Self::Cycle { count, .. }
            | Self::Similar { count, .. } => *count,
        }
    }

    pub(super) fn saturation(&self) -> u32 {
        match self {
            Self::Identical { .. } | Self::Similar { .. } => 64,
            Self::Cycle { period, .. } => 64 / period,
        }
    }

    pub(super) fn notice(&self) -> Box<str> {
        match self {
            Self::Identical { tool, count, .. } => {
                let count_text = count.to_string().into_boxed_str();
                render_template(
                    IDENTICAL_NOTICE,
                    &[("<tool>", tool), ("<count>", &count_text)],
                )
            }
            Self::Similar {
                tool,
                count,
                similarity,
                ..
            } => {
                let count_text = count.to_string().into_boxed_str();
                let percent = (similarity * 100.0).round().to_string().into_boxed_str();
                render_template(
                    SIMILAR_NOTICE,
                    &[
                        ("<tool>", tool),
                        ("<count>", &count_text),
                        ("<percent>", &percent),
                    ],
                )
            }
            Self::Cycle {
                tools,
                period,
                count,
                ..
            } => {
                let cycle = tools
                    .iter()
                    .map(AsRef::as_ref)
                    .collect::<Vec<_>>()
                    .join(" -> ");
                let mut text = String::with_capacity(
                    CYCLE_NOTICE_HEAD.len() + cycle.len() + CYCLE_NOTICE_TAIL.len(),
                );
                text.push_str(CYCLE_NOTICE_HEAD);
                text.push_str(&cycle);
                text.push_str(CYCLE_NOTICE_TAIL);
                let count_text = count.to_string().into_boxed_str();
                let period_text = period.to_string().into_boxed_str();
                render_template(
                    &text,
                    &[("<count>", &count_text), ("<period>", &period_text)],
                )
            }
        }
    }
}

/// Runs detectors against the record ending at the current call in order:
/// identical, cycle, similar.
pub(super) fn detect(
    records: &VecDeque<ToolCallRecord>,
    current: &Value,
) -> Result<Option<Detection>, GuardError> {
    if let Some(detection) = detect_identical(records) {
        return Ok(Some(detection));
    }
    if let Some(detection) = detect_cycle(records) {
        return Ok(Some(detection));
    }
    detect_similar(records, current)
}

/// Three consecutive equal signatures.
fn detect_identical(records: &VecDeque<ToolCallRecord>) -> Option<Detection> {
    let last = records.back()?;
    let count = records
        .iter()
        .rev()
        .take_while(|record| record.signature == last.signature)
        .count();
    if count < 3 {
        return None;
    }
    Some(Detection::Identical {
        fingerprint: last.signature.clone(),
        tool: last.tool.clone(),
        count: u32::try_from(count).ok()?,
    })
}

/// An exact repeating signature sequence with period 2 through 6 and three
/// complete repetitions. The fingerprint is the rotation-minimal signature
/// sequence so a cycle admits one gate whatever its entry rotation.
fn detect_cycle(records: &VecDeque<ToolCallRecord>) -> Option<Detection> {
    let len = records.len();
    for period in 2..=6 {
        if len < period * 3 {
            continue;
        }
        let pattern_start = len - period;
        if !is_minimal_period(records, pattern_start, period) {
            continue;
        }
        let mut repetitions = 1_usize;
        while repetitions < len / period && repetitions < RECORD_CAPACITY / period {
            let earlier = len - (repetitions + 1) * period;
            let same = (0..period).all(|offset| {
                records
                    .get(earlier + offset)
                    .zip(records.get(pattern_start + offset))
                    .is_some_and(|(left, right)| left.signature == right.signature)
            });
            if !same {
                break;
            }
            repetitions += 1;
        }
        if repetitions < 3 {
            continue;
        }
        let mut rotations = (0..period)
            .map(|offset| {
                (0..period)
                    .filter_map(|index| {
                        records
                            .get(pattern_start + (index + offset) % period)
                            .map(|record| record.signature.as_ref())
                    })
                    .collect::<Vec<_>>()
            })
            .collect::<Vec<_>>();
        rotations.sort();
        let fingerprint = rotations.first()?.join("\u{1f}");
        let tools = (0..period)
            .filter_map(|index| records.get(pattern_start + index))
            .map(|record| record.tool.clone())
            .collect();
        return Some(Detection::Cycle {
            fingerprint: fingerprint.into_boxed_str(),
            tools,
            period: u32::try_from(period).ok()?,
            count: u32::try_from(repetitions).ok()?,
        });
    }
    None
}

/// Rejects a pattern that is itself a repetition of a shorter period.
fn is_minimal_period(
    records: &VecDeque<ToolCallRecord>,
    pattern_start: usize,
    period: usize,
) -> bool {
    !(1..period).any(|candidate| {
        period.is_multiple_of(candidate)
            && (0..period).all(|index| {
                records
                    .get(pattern_start + index)
                    .zip(records.get(pattern_start + index % candidate))
                    .is_some_and(|(left, right)| left.signature == right.signature)
            })
    })
}

/// Five consecutive calls with the same tool whose mean adjacent Dice score
/// over bigram multisets is at least 0.85, excluding five equal argument
/// strings and, for `read`, five different `read.path` targets.
fn detect_similar(
    records: &VecDeque<ToolCallRecord>,
    current: &Value,
) -> Result<Option<Detection>, GuardError> {
    if records.len() < 5 {
        return Ok(None);
    }
    let window = records.iter().skip(records.len() - 5).collect::<Vec<_>>();
    let Some(first) = window.first() else {
        return Ok(None);
    };
    let tool = &first.tool;
    if !window.iter().all(|record| record.tool == *tool) {
        return Ok(None);
    }
    if window
        .windows(2)
        .all(|pair| pair[0].canonical == pair[1].canonical)
    {
        return Ok(None);
    }
    if tool.as_ref() == "read" {
        let mut paths = Vec::with_capacity(5);
        for (index, record) in window.iter().enumerate() {
            let path = if index == 4 {
                read_path(current).map(str::to_owned)
            } else {
                let value: Value =
                    sonic_rs::from_str(&record.canonical).map_err(GuardError::json)?;
                read_path(&value).map(str::to_owned)
            };
            paths.push(path);
        }
        let all_different = paths
            .iter()
            .enumerate()
            .all(|(index, path)| paths[..index].iter().all(|earlier| earlier != path));
        if all_different {
            return Ok(None);
        }
    }
    let similarity = window
        .windows(2)
        .map(|pair| dice(&pair[0].canonical, &pair[1].canonical))
        .sum::<f64>()
        / 4.0;
    if similarity < 0.85 {
        return Ok(None);
    }
    let count = records
        .iter()
        .rev()
        .take_while(|record| record.tool == *tool)
        .count();
    let count = u32::try_from(count).map_err(GuardError::json)?;
    Ok(Some(Detection::Similar {
        fingerprint: format!("similar:{tool}").into_boxed_str(),
        tool: tool.clone(),
        count,
        similarity,
    }))
}

/// Reads the single target field the similar detector understands.
pub(super) fn read_path(value: &Value) -> Option<&str> {
    value
        .as_object()
        .and_then(|object| object.get(&"path"))
        .and_then(|path| path.as_str())
}

/// Dice score over multisets of adjacent Unicode scalar-value bigrams.
fn dice(left: &str, right: &str) -> f64 {
    let left_bigrams = bigrams(left);
    let right_bigrams = bigrams(right);
    let left_count = left_bigrams
        .values()
        .copied()
        .fold(0_u32, u32::saturating_add);
    let right_count = right_bigrams
        .values()
        .copied()
        .fold(0_u32, u32::saturating_add);
    let intersection = left_bigrams.iter().fold(0_u32, |total, (bigram, count)| {
        total.saturating_add((*count).min(right_bigrams.get(bigram).copied().unwrap_or_default()))
    });
    let denominator = left_count.saturating_add(right_count);
    if denominator == 0 {
        1.0
    } else {
        2.0 * f64::from(intersection) / f64::from(denominator)
    }
}

/// Counts adjacent Unicode scalar-value bigrams.
fn bigrams(value: &str) -> HashMap<(char, char), u32> {
    let mut counts = HashMap::new();
    let mut chars = value.chars();
    let Some(mut previous) = chars.next() else {
        return counts;
    };
    for current in chars {
        let count = counts.entry((previous, current)).or_insert(0_u32);
        *count = count.saturating_add(1);
        previous = current;
    }
    counts
}
