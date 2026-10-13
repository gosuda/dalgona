// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0
//! Sleep-wait classifier: pure shell-command policy over compiled regexes.

use regex_automata::meta::Regex;

/// Which sleep shape classified the command.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum SleepRule {
    R1,
    R2,
    R3,
    R4,
}

/// One classified sleep-wait with its duration in seconds.
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct SleepWait {
    pub rule: SleepRule,
    pub seconds: f64,
}

/// Fixed sleep expressions compiled once when the extension is built.
#[derive(Debug)]
pub(crate) struct SleepClassifier {
    wrapper: Regex,
    power_management: Regex,
    sleep_call: Regex,
    pure: Regex,
    leading: Regex,
    loop_call: Regex,
    trailing: Regex,
}

const WRAPPER: &str =
    r"(?i)^(?:env\s+(?:[A-Za-z0-9_.]+=\S*\s+)*)?(?:/(?:usr/)?bin/)?(?:ba|z|da|k)?sh\s+-[a-z]*c\s+";
const POWER_MANAGEMENT: &str = r"\b(?:pmset|systemsetup|caffeinate|displaysleep|disksleep)\b";
const SLEEP_CALL: &str = r"(?:^|[^A-Za-z0-9_.\-/])(?:/(?:usr/)?bin/)?sleep\s+([0-9]+(?:\.[0-9]+)?)";
const PURE: &str = r"^sleep\s+[0-9]+(?:\.[0-9]+)?$";
const LEADING: &str = r"^\(?\s*(?:/(?:usr/)?bin/)?sleep\s+[0-9]+(?:\.[0-9]+)?\s*(?:[;&|]|$)";
const LOOP: &str =
    r"\b(?:while|until|for)\b(?s:.*?)[^A-Za-z0-9_.\-/](?:/(?:usr/)?bin/)?sleep\s+[0-9]";
const TRAILING: &str = r"[;&|]\s*(?:/(?:usr/)?bin/)?sleep\s+[0-9]+(?:\.[0-9]+)?\s*\)?\s*$";

fn compile(pattern: &str) -> Result<Regex, Box<regex_automata::meta::BuildError>> {
    Regex::new(pattern).map_err(Box::new)
}
impl SleepClassifier {
    pub(crate) fn new() -> Result<Self, Box<regex_automata::meta::BuildError>> {
        Ok(Self {
            wrapper: compile(WRAPPER)?,
            power_management: compile(POWER_MANAGEMENT)?,
            sleep_call: compile(SLEEP_CALL)?,
            pure: compile(PURE)?,
            leading: compile(LEADING)?,
            loop_call: compile(LOOP)?,
            trailing: compile(TRAILING)?,
        })
    }

    /// Tests the original command after removing at most three leading shell
    /// wrappers, peeling one matching outer quote pair per wrapper. Power
    /// management commands never classify. Rules R1, R2, and R4 require at
    /// least 10 seconds; R3 requires at least 2 seconds.
    pub(crate) fn classify(&self, original: &str) -> Option<SleepWait> {
        let mut command = original;
        for _ in 0..3 {
            let Some(wrapper) = self.wrapper.find(command) else {
                break;
            };
            command = strip_outer_quotes(&command[wrapper.end()..]);
        }
        if self.power_management.is_match(command) {
            return None;
        }
        if self.pure.is_match(command) {
            return classify_sleep_match(&self.sleep_call, command, SleepRule::R1, 10.0);
        }
        if self.leading.is_match(command) {
            return classify_sleep_match(&self.sleep_call, command, SleepRule::R2, 10.0);
        }
        if let Some(found) = self.loop_call.find(command) {
            let digit = found.end().checked_sub(1)?;
            let seconds = parse_decimal_prefix(&command[digit..])?;
            return (seconds >= 2.0).then_some(SleepWait {
                rule: SleepRule::R3,
                seconds,
            });
        }
        if let Some(found) = self.trailing.find(command) {
            return classify_sleep_match(
                &self.sleep_call,
                &command[found.start()..found.end()],
                SleepRule::R4,
                10.0,
            );
        }
        None
    }
}

/// Removes one matching outer single- or double-quote pair, if present.
fn strip_outer_quotes(command: &str) -> &str {
    let bytes = command.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'\'' | b'"') && bytes[bytes.len() - 1] == bytes[0] {
        &command[1..bytes.len() - 1]
    } else {
        command
    }
}

/// Classifies the first sleep call in a matched span when its duration meets
/// the rule threshold.
fn classify_sleep_match(
    sleep_call: &Regex,
    command: &str,
    rule: SleepRule,
    minimum: f64,
) -> Option<SleepWait> {
    let found = sleep_call.find(command)?;
    let number = command[found.start()..found.end()]
        .split_whitespace()
        .last()?;
    let seconds = number.parse::<f64>().ok()?;
    (seconds >= minimum).then_some(SleepWait { rule, seconds })
}

/// Parses a leading decimal without reading past its end.
fn parse_decimal_prefix(value: &str) -> Option<f64> {
    let bytes = value.as_bytes();
    let mut end = 0;
    let mut has_digit = false;
    let mut has_dot = false;
    while let Some(byte) = bytes.get(end).copied() {
        if byte.is_ascii_digit() {
            has_digit = true;
            end += 1;
        } else if byte == b'.' && !has_dot {
            has_dot = true;
            end += 1;
        } else {
            break;
        }
    }
    if !has_digit || value.get(..end)?.ends_with('.') {
        return None;
    }
    value.get(..end)?.parse().ok()
}
