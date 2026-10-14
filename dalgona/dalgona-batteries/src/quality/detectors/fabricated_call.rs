// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

use dal_core::ext::{InterruptMode, Name, RegistrationError, RepeatMode, RuleRecord, Scope};

/// Name of the fabricated-tool-call rule.
pub const RULE_NAME: &str = "fabricated-unavailable-tool-call";
/// Reminder text injected when the rule fires.
pub const RULE_BODY: &str = "Your previous output imitated an unavailable tool call as inert text instead of taking action. Redo the interrupted step now with your real tools, such as patch for file changes. Do not print or imitate unavailable-tool transcript envelopes.";
/// Patterns that match an imitated unavailable-tool transcript.
pub const RULE_PATTERNS: &[&str] = &[
    "(?i)<\\s*unavailable-tool-call\\b",
    "(?i)\\[called\\s+tool\\s+[\"'][^\"'\\r\\n]+[\"']\\s+\\(no\\s+longer\\s+available\\s+in\\s+this\\s+session\\)",
];

/// Builds the interrupting rule record for the fabricated-tool-call lane.
///
/// # Errors
///
/// Returns a [`RegistrationError`] if the rule name is invalid.
pub fn rule() -> Result<RuleRecord, RegistrationError> {
    let name = Name::parse(RULE_NAME)?;
    Ok(RuleRecord {
        name,
        patterns: RULE_PATTERNS
            .iter()
            .map(|pattern| (*pattern).into())
            .collect(),
        text: RULE_BODY.into(),
        judge: None,
        scope: Some(Scope {
            text: true,
            thinking: false,
            tool: false,
            named_tools: Vec::new(),
        }),
        globs: None,
        agents: None,
        mode: Some(InterruptMode::Always),
        repeat_mode: Some(RepeatMode::AfterGap),
        repeat_gap: Some(1),
        always_apply: false,
        report: false,
        enabled: true,
    })
}
