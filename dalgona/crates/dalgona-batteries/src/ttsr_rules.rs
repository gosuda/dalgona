// SPDX-License-Identifier: LicenseRef-Sustainable-Use-1.0

//! The bundled TTSR rule sets of dalgona: seven compiled Markdown packs
//! plus the compiled `detectors` set name.

use dal_core::ext::{InterruptMode, RepeatMode, RuleRecord, Scope};
use dal_core::{Name, RegistrationError, ServiceSet};

/// The eight set names, in the order the default list and docs page print.
pub const SETS: &[&str] = &[
    "steer",
    "compact",
    "stop",
    "docs",
    "atlas-v2",
    "project-workflow",
    "git-commit",
    "detectors",
];

/// The seven Markdown pack names; `detectors` has no pack directory.
pub const PACKS: &[&str] = &[
    "steer",
    "compact",
    "stop",
    "docs",
    "atlas-v2",
    "project-workflow",
    "git-commit",
];

/// The four rule names of the compiled `detectors` set, which the quality
/// battery registers. This list exists for name checks, docs, and the gate
/// test; this module does not register them.
pub const DETECTOR_RULES: &[&str] = &[
    "collapse-repetition",
    "control-token-leak",
    "repetitive-turns",
    "fabricated-unavailable-tool-call",
];

/// The value `[rule_sets] enabled` takes when the config file omits the
/// section. It is held in code, not in `defaults.toml`.
pub const DEFAULT_ENABLED: &[&str] = SETS;

/// Hint line for a non-list `[rule_sets] enabled` config error.
pub const NOT_A_LIST_HINT: &str = "Use a list of set names in [\"steer\", ...].";

/// A set-list validation error.
#[derive(Debug, thiserror::Error)]
pub enum RuleSetsError {
    /// A requested set name is not shipped by dalgona.
    #[error(
        "dalgona: config.toml: rule_sets.enabled [\"{set}\"] is invalid\n{hint}",
        set = .0,
        hint = rule_sets_hint(),
    )]
    UnknownSet(String),
}

impl RuleSetsError {
    /// The second line of the config error, per the two-line config shape.
    #[must_use]
    pub fn hint(&self) -> String {
        format!("{}\n", rule_sets_hint())
    }
}

fn rule_sets_hint() -> String {
    format!("Use a set name from {}.", SETS.join(", "))
}

/// Validates `[rule_sets] enabled` after the caller decoded a list.
/// Duplicate entries are idempotent; an unknown name fails startup.
///
/// # Errors
/// Returns [`RuleSetsError`] for a name outside [`SETS`].
pub fn validate_enabled(enabled: &[String]) -> Result<(), RuleSetsError> {
    for name in enabled {
        if !SETS.contains(&name.as_str()) {
            return Err(RuleSetsError::UnknownSet(name.clone()));
        }
    }
    Ok(())
}

/// One compiled rule declaration: the declaration values with the rule's
/// Markdown asset, whose body carries the reminder text.
struct RuleDecl {
    name: &'static str,
    pattern: Option<&'static str>,
    file: &'static str,
    scope: Option<ScopeDecl>,
    mode: Option<InterruptMode>,
    repeat_gap: Option<u16>,
}

/// The const-time scope shape; [`record`] expands it to [`Scope`].
#[derive(Clone, Copy)]
enum ScopeDecl {
    /// Assistant text.
    Text,
    /// The named tools' arguments and results.
    Tool(&'static [&'static str]),
}

fn scope(decl: Option<ScopeDecl>) -> Result<Option<Scope>, RegistrationError> {
    Ok(match decl {
        None => None,
        Some(ScopeDecl::Text) => Some(Scope {
            text: true,
            thinking: false,
            tool: false,
            named_tools: Vec::new(),
        }),
        Some(ScopeDecl::Tool(named)) => Some(Scope {
            text: false,
            thinking: false,
            tool: true,
            named_tools: named
                .iter()
                .copied()
                .map(Name::parse)
                .collect::<Result<_, _>>()?,
        }),
    })
}

const fn prose_rule(
    name: &'static str,
    pattern: &'static str,
    file: &'static str,
    gap: u16,
) -> RuleDecl {
    RuleDecl {
        name,
        pattern: Some(pattern),
        file,
        scope: Some(ScopeDecl::Text),
        mode: Some(InterruptMode::ProseOnly),
        repeat_gap: Some(gap),
    }
}

const fn tool_rule(
    name: &'static str,
    pattern: &'static str,
    file: &'static str,
    tools: &'static [&'static str],
) -> RuleDecl {
    RuleDecl {
        name,
        pattern: Some(pattern),
        file,
        scope: Some(ScopeDecl::Tool(tools)),
        mode: Some(InterruptMode::ToolOnly),
        repeat_gap: Some(1),
    }
}

const fn always_rule(name: &'static str, file: &'static str) -> RuleDecl {
    RuleDecl {
        name,
        pattern: None,
        file,
        scope: None,
        mode: None,
        repeat_gap: None,
    }
}

/// The seven packs' declarations, in canonical pack and file-name order.
const PACK_RULES: &[(&str, &[RuleDecl])] = &[
    (
        "steer",
        &[
            prose_rule(
                "steer-no-apologies",
                r"(?i)\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:'re| are) (?:absolutely )?right)\b",
                include_str!("ttsr_rules/steer/steer-no-apologies.md"),
                10,
            ),
            prose_rule(
                "steer-no-meta",
                r"(?i)(?:\bas an ai (?:language )?model\b)|(?:\bi do not have (?:access|the ability) to\b)",
                include_str!("ttsr_rules/steer/steer-no-meta.md"),
                10,
            ),
            prose_rule(
                "steer-no-restate",
                r"(?i)\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\b",
                include_str!("ttsr_rules/steer/steer-no-restate.md"),
                10,
            ),
        ],
    ),
    (
        "compact",
        &[
            prose_rule(
                "compact-cite-files",
                r"(?i)\bas (?:mentioned|said|discussed|described) earlier\b",
                include_str!("ttsr_rules/compact/compact-cite-files.md"),
                10,
            ),
            prose_rule(
                "compact-no-context-loss",
                r"(?i)\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\b",
                include_str!("ttsr_rules/compact/compact-no-context-loss.md"),
                5,
            ),
            always_rule(
                "compact-state-on-disk",
                include_str!("ttsr_rules/compact/compact-state-on-disk.md"),
            ),
        ],
    ),
    (
        "stop",
        &[
            prose_rule(
                "stop-act-dont-offer",
                r"(?i)\b(?:shall i|should i|do you want me to|would you like me to) (?:continue|proceed|fix|implement|run)\b",
                include_str!("ttsr_rules/stop/stop-act-dont-offer.md"),
                5,
            ),
            prose_rule(
                "stop-evidence-before-done",
                r"(?i)\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\b",
                include_str!("ttsr_rules/stop/stop-evidence-before-done.md"),
                5,
            ),
            prose_rule(
                "stop-finish-the-work",
                r"(?i)(?:\bi(?:'ll| will) leave\b)|(?:\bthe rest (?:is|should be) straightforward\b)",
                include_str!("ttsr_rules/stop/stop-finish-the-work.md"),
                10,
            ),
        ],
    ),
    (
        "docs",
        &[
            RuleDecl {
                name: "docs-contract-markers",
                pattern: Some(
                    r"(?i)(?:\+.{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b)|(?:\+.{0,200}\b(?:deprecated|breaking change)\b)",
                ),
                file: include_str!("ttsr_rules/docs/docs-contract-markers.md"),
                scope: Some(ScopeDecl::Tool(&["patch"])),
                mode: Some(InterruptMode::Never),
                repeat_gap: Some(10),
            },
            always_rule(
                "docs-update-with-change",
                include_str!("ttsr_rules/docs/docs-update-with-change.md"),
            ),
        ],
    ),
    (
        "atlas-v2",
        &[
            prose_rule(
                "atlas-v2-no-workaround",
                r"(?i)\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\b",
                include_str!("ttsr_rules/atlas-v2/atlas-v2-no-workaround.md"),
                10,
            ),
            always_rule(
                "atlas-v2-read-before-edit",
                include_str!("ttsr_rules/atlas-v2/atlas-v2-read-before-edit.md"),
            ),
        ],
    ),
    (
        "project-workflow",
        &[
            always_rule(
                "project-workflow-agents-md-binding",
                include_str!("ttsr_rules/project-workflow/project-workflow-agents-md-binding.md"),
            ),
            prose_rule(
                "project-workflow-slice-first",
                r"(?i)\b(?:rewrite|replace|restructure) (?:the )?(?:whole|entire|all of)\b",
                include_str!("ttsr_rules/project-workflow/project-workflow-slice-first.md"),
                10,
            ),
            prose_rule(
                "project-workflow-write-it-down",
                r"(?i)\bi(?:'ll| will) (?:keep|remember) (?:that|this|it) in mind\b",
                include_str!("ttsr_rules/project-workflow/project-workflow-write-it-down.md"),
                5,
            ),
        ],
    ),
    (
        "git-commit",
        &[
            tool_rule(
                "git-commit-no-force-push",
                r"(?i)(?:\bgit push\b.{0,200}\s--force(?:\s|$))|(?:\bgit push\b.{0,200}\s-f(?:\s|$))",
                include_str!("ttsr_rules/git-commit/git-commit-no-force-push.md"),
                &["exec"],
            ),
            tool_rule(
                "git-commit-no-placeholder-message",
                r#"(?i)\bgit commit\b.{0,200}-m\s+["'](wip|tmp|temp|fix|update|stuff|changes|misc)["'](\s|$)"#,
                include_str!("ttsr_rules/git-commit/git-commit-no-placeholder-message.md"),
                &["exec"],
            ),
            tool_rule(
                "git-commit-no-secrets",
                r#"(?i)(?:api[_-]?key|secret|password|token|credential)["']?\s*[:=]\s*["'][A-Za-z0-9+/_=-]{16,}["']"#,
                include_str!("ttsr_rules/git-commit/git-commit-no-secrets.md"),
                &["patch"],
            ),
        ],
    ),
];

/// The rule text of one Markdown asset: the body after its front matter.
fn rule_text(file: &str) -> &str {
    match file.strip_prefix("---\n") {
        Some(rest) => match rest.split_once("\n---\n") {
            Some((_, body)) => body,
            None => file,
        },
        None => file,
    }
}

fn record(decl: &RuleDecl) -> Result<RuleRecord, RegistrationError> {
    Ok(RuleRecord {
        name: Name::parse(decl.name)?,
        patterns: decl
            .pattern
            .map(|pattern| vec![Box::from(pattern)])
            .unwrap_or_default(),
        text: rule_text(decl.file).into(),
        judge: None,
        scope: scope(decl.scope)?,
        globs: None,
        agents: None,
        mode: decl.mode,
        repeat_mode: decl.repeat_gap.map(|_| RepeatMode::AfterGap),
        repeat_gap: decl.repeat_gap,
        always_apply: decl.pattern.is_none(),
        report: false,
        enabled: true,
    })
}

/// Builds the bundled TTSR extension with the rules of `enabled`.
///
/// `enabled` must already pass [`validate_enabled`]; the caller owns the
/// config error. The extension carries one registration name; each selected
/// pack's rules register through it. `detectors` selects no pack here: the
/// quality battery owns those compiled lanes.
///
/// # Errors
/// Returns the extension runtime's registration error.
pub fn ttsr_rules(enabled: &[String]) -> Result<dal_agent::ext::Extension, RegistrationError> {
    let mut builder = dal_agent::ext::ExtensionBuilder::new(
        "ttsr-rules",
        env!("CARGO_PKG_VERSION"),
        ServiceSet::EMPTY,
    )?
    .with_origin(dal_core::Origin::Bundled, None);
    for (set, rules) in PACK_RULES {
        if !enabled.iter().any(|name| name == set) {
            continue;
        }
        for decl in *rules {
            builder = builder.rule(record(decl)?);
        }
    }
    builder.build()
}

/// The text of the `dalgona://rules` manual page.
pub const RULES_DOC: &str = concat!(
    "# rules\n\n",
    "Dalgona ships seven bundled rule sets plus the compiled detector lanes.\n",
    "The `[rule_sets] enabled` list selects them; an unknown name fails\n",
    "startup, and duplicate entries are idempotent.\n\n",
    "| set | rules |\n",
    "|---|---|\n",
    "| steer | steer-no-apologies, steer-no-meta, steer-no-restate |\n",
    "| compact | compact-cite-files, compact-no-context-loss, compact-state-on-disk |\n",
    "| stop | stop-act-dont-offer, stop-evidence-before-done, stop-finish-the-work |\n",
    "| docs | docs-contract-markers, docs-update-with-change |\n",
    "| atlas-v2 | atlas-v2-no-workaround, atlas-v2-read-before-edit |\n",
    "| project-workflow | project-workflow-agents-md-binding, project-workflow-slice-first, project-workflow-write-it-down |\n",
    "| git-commit | git-commit-no-force-push, git-commit-no-placeholder-message, git-commit-no-secrets |\n",
    "| detectors | collapse-repetition, control-token-leak, repetitive-turns, fabricated-unavailable-tool-call |\n",
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundled_rule_text_excludes_markdown_front_matter() {
        for set in PACKS {
            let extension = ttsr_rules(&[(*set).to_owned()]).expect("the shipped set registers");
            assert!(!extension.rules().is_empty(), "{set}");
            assert!(
                extension
                    .rules()
                    .iter()
                    .all(|rule| !rule.text.starts_with("---\n")),
                "{set}"
            );
        }
    }

    #[test]
    fn known_set_names_are_idempotent_and_unknown_names_fail() {
        assert!(
            validate_enabled(
                &SETS
                    .iter()
                    .map(|name| (*name).to_owned())
                    .collect::<Vec<_>>()
            )
            .is_ok()
        );
        assert!(validate_enabled(&["stop".to_owned(), "stop".to_owned()]).is_ok());
        assert!(validate_enabled(&["nope".to_owned()]).is_err());
    }
}
