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
        "dalgona: dal.toml: rule_sets.enabled [\"{set}\"] is invalid\n{hint}",
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
    patterns: &'static [&'static str],
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
    patterns: &'static [&'static str],
    file: &'static str,
    gap: u16,
) -> RuleDecl {
    RuleDecl {
        name,
        patterns,
        file,
        scope: Some(ScopeDecl::Text),
        mode: Some(InterruptMode::ProseOnly),
        repeat_gap: Some(gap),
    }
}

const fn tool_rule(
    name: &'static str,
    patterns: &'static [&'static str],
    file: &'static str,
    tools: &'static [&'static str],
) -> RuleDecl {
    RuleDecl {
        name,
        patterns,
        file,
        scope: Some(ScopeDecl::Tool(tools)),
        mode: Some(InterruptMode::ToolOnly),
        repeat_gap: Some(1),
    }
}

const fn always_rule(name: &'static str, file: &'static str) -> RuleDecl {
    RuleDecl {
        name,
        patterns: &[],
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
                &[
                    r"(?i)\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:'re| are) (?:absolutely )?right)\b",
                ],
                include_str!("ttsr_rules/steer/steer-no-apologies.md"),
                10,
            ),
            prose_rule(
                "steer-no-meta",
                &[
                    r"(?i)(?:\bas an ai (?:language )?model\b)|(?:\bi do not have (?:access|the ability) to\b)",
                ],
                include_str!("ttsr_rules/steer/steer-no-meta.md"),
                10,
            ),
            prose_rule(
                "steer-no-restate",
                &[
                    r"(?i)\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\b",
                ],
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
                &[r"(?i)\bas (?:mentioned|said|discussed|described) earlier\b"],
                include_str!("ttsr_rules/compact/compact-cite-files.md"),
                10,
            ),
            prose_rule(
                "compact-no-context-loss",
                &[
                    r"(?i)\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\b",
                ],
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
                &[
                    r"(?i)\b(?:shall i|should i|do you want me to|would you like me to) (?:continue|proceed|fix|implement|run)\b",
                ],
                include_str!("ttsr_rules/stop/stop-act-dont-offer.md"),
                5,
            ),
            prose_rule(
                "stop-evidence-before-done",
                &[
                    r"(?i)\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\b",
                ],
                include_str!("ttsr_rules/stop/stop-evidence-before-done.md"),
                5,
            ),
            prose_rule(
                "stop-finish-the-work",
                &[
                    r"(?i)(?:\bi(?:'ll| will) leave\b)|(?:\bthe rest (?:is|should be) straightforward\b)",
                ],
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
                patterns: &[
                    r"(?i)\+[^\n+]{0,200}\bversion [0-9]{1,4}\.[0-9]{1,3}\.[0-9]{1,3}\b",
                    r"(?i)\+[^\n+]{0,200}\b(?:deprecated|breaking change)\b",
                ],
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
                &[r"(?i)\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\b"],
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
                &[r"(?i)\b(?:rewrite|replace|restructure) (?:the )?(?:whole|entire|all of)\b"],
                include_str!("ttsr_rules/project-workflow/project-workflow-slice-first.md"),
                10,
            ),
            prose_rule(
                "project-workflow-write-it-down",
                &[r"(?i)\bi(?:'ll| will) (?:keep|remember) (?:that|this|it) in mind\b"],
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
                &[
                    r"(?i)(?:\bgit push\b.{0,200}\s--force(?:\s|$))|(?:\bgit push\b.{0,200}\s-f(?:\s|$))",
                ],
                include_str!("ttsr_rules/git-commit/git-commit-no-force-push.md"),
                &["exec"],
            ),
            tool_rule(
                "git-commit-no-placeholder-message",
                &[
                    r#"(?i)\bgit commit\b.{0,200}-m\s+["'](wip|tmp|temp|fix|update|stuff|changes|misc)["'](\s|$)"#,
                ],
                include_str!("ttsr_rules/git-commit/git-commit-no-placeholder-message.md"),
                &["exec"],
            ),
            tool_rule(
                "git-commit-no-secrets",
                &[
                    r#"(?i)(?:api[_-]?key|secret|password|token|credential)["']?\s*[:=]\s*["'][A-Za-z0-9+/_=-]{16,}["']"#,
                ],
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
            .patterns
            .iter()
            .map(|pattern| Box::from(*pattern))
            .collect(),
        text: rule_text(decl.file).into(),
        judge: None,
        scope: scope(decl.scope)?,
        globs: None,
        agents: None,
        mode: decl.mode,
        repeat_mode: decl.repeat_gap.map(|_| RepeatMode::AfterGap),
        repeat_gap: decl.repeat_gap,
        always_apply: decl.patterns.is_empty(),
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

/// The text of the `dalgona://rules` manual page: the full set inventory
/// with origin, scope, mode, repeat, pattern, and body per rule, plus the
/// byte budgets and the swap surface. A test in this module compares the
/// page against the declarations above so the two cannot drift.
pub const RULES_DOC: &str = concat!(
    "# rules\n\n",
    "Dalgona ships seven bundled rule sets plus the compiled `detectors`\n",
    "set: 23 rules in total. The `[rule_sets] enabled` list selects sets;\n",
    "an unknown name fails startup, and duplicate entries are idempotent.\n",
    "Every loaded rule's exact source is readable at `rule://<name>`.\n\n",
    "## Origins\n\n",
    "A rule's origin names where it comes from: a user file path under\n",
    "`<data-root>/rules/`, a project path relative to the workspace root\n",
    "(`<workspace>/.dal/rules/`), or `plugin:<extension>` for a rule that an\n",
    "extension registers in code. The seven Markdown sets register through\n",
    "the `ttsr-rules` extension, so their rules print `plugin:ttsr-rules`;\n",
    "the set is the rule-name prefix, and each rule's compiled-in source is\n",
    "the `<set>/rules/<rule>.md` file in its set table. The `detectors`\n",
    "text rule registers through the `quality` extension (`plugin:quality`);\n",
    "the three detector lanes are compiled code, not rules in the rule\n",
    "table, and print no origin. A `rule_fired` journal record of a rule\n",
    "registered in code carries its label in `origin`.\n\n",
    "## Set inventory\n\n",
    "### steer\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| steer-no-apologies | steer/rules/steer-no-apologies.md | text | prose-only | after-gap 10 |\n",
    "| steer-no-meta | steer/rules/steer-no-meta.md | text | prose-only | after-gap 10 |\n",
    "| steer-no-restate | steer/rules/steer-no-restate.md | text | prose-only | after-gap 10 |\n\n",
    "#### steer-no-apologies\n\ncondition: `(?i)\\b(?:i apologize|my apologies|sorry for (?:the )?(?:confusion|the mistake)|you(?:'re| are) (?:absolutely )?right)\\b`\n\n",
    include_str!("ttsr_rules/steer/steer-no-apologies.md"),
    "\n#### steer-no-meta\n\ncondition: `(?i)(?:\\bas an ai (?:language )?model\\b)|(?:\\bi do not have (?:access|the ability) to\\b)`\n\n",
    include_str!("ttsr_rules/steer/steer-no-meta.md"),
    "\n#### steer-no-restate\n\ncondition: `(?i)\\blet me (?:restate|repeat|recap|summarize) (?:the |your )(?:task|request|question|instructions)\\b`\n\n",
    include_str!("ttsr_rules/steer/steer-no-restate.md"),
    "\n### compact\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| compact-cite-files | compact/rules/compact-cite-files.md | text | prose-only | after-gap 10 |\n",
    "| compact-no-context-loss | compact/rules/compact-no-context-loss.md | text | prose-only | after-gap 5 |\n",
    "| compact-state-on-disk | compact/rules/compact-state-on-disk.md | always applied | - | - |\n\n",
    "#### compact-cite-files\n\ncondition: `(?i)\\bas (?:mentioned|said|discussed|described) earlier\\b`\n\n",
    include_str!("ttsr_rules/compact/compact-cite-files.md"),
    "\n#### compact-no-context-loss\n\ncondition: `(?i)\\b(?:i (?:do not|cannot) (?:recall|remember)|i (?:lost|no longer have) (?:access to )?(?:the )?(?:earlier|previous) (?:context|conversation))\\b`\n\n",
    include_str!("ttsr_rules/compact/compact-no-context-loss.md"),
    "\n#### compact-state-on-disk\n\nAlways applied; no condition.\n\n",
    include_str!("ttsr_rules/compact/compact-state-on-disk.md"),
    "\n### stop\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| stop-act-dont-offer | stop/rules/stop-act-dont-offer.md | text | prose-only | after-gap 5 |\n",
    "| stop-evidence-before-done | stop/rules/stop-evidence-before-done.md | text | prose-only | after-gap 5 |\n",
    "| stop-finish-the-work | stop/rules/stop-finish-the-work.md | text | prose-only | after-gap 10 |\n\n",
    "#### stop-act-dont-offer\n\ncondition: `(?i)\\b(?:shall i|should i|do you want me to|would you like me to) (?:continue|proceed|fix|implement|run)\\b`\n\n",
    include_str!("ttsr_rules/stop/stop-act-dont-offer.md"),
    "\n#### stop-evidence-before-done\n\ncondition: `(?i)\\b(?:all (?:tests|checks) (?:pass|passed)|everything (?:is )?work(?:ing|s))\\b`\n\n",
    include_str!("ttsr_rules/stop/stop-evidence-before-done.md"),
    "\n#### stop-finish-the-work\n\ncondition: `(?i)(?:\\bi(?:'ll| will) leave\\b)|(?:\\bthe rest (?:is|should be) straightforward\\b)`\n\n",
    include_str!("ttsr_rules/stop/stop-finish-the-work.md"),
    "\n### docs\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| docs-contract-markers | docs/rules/docs-contract-markers.md | patch calls | never | after-gap 10 |\n",
    "| docs-update-with-change | docs/rules/docs-update-with-change.md | always applied | - | - |\n\n",
    "#### docs-contract-markers\n\nconditions, exactly:\n\n- `(?i)\\+[^\\n+]{0,200}\\bversion [0-9]{1,4}\\.[0-9]{1,3}\\.[0-9]{1,3}\\b`\n- `(?i)\\+[^\\n+]{0,200}\\b(?:deprecated|breaking change)\\b`\n\n",
    include_str!("ttsr_rules/docs/docs-contract-markers.md"),
    "\n#### docs-update-with-change\n\nAlways applied; no condition.\n\n",
    include_str!("ttsr_rules/docs/docs-update-with-change.md"),
    "\n### atlas-v2\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| atlas-v2-no-workaround | atlas-v2/rules/atlas-v2-no-workaround.md | text | prose-only | after-gap 10 |\n",
    "| atlas-v2-read-before-edit | atlas-v2/rules/atlas-v2-read-before-edit.md | always applied | - | - |\n\n",
    "#### atlas-v2-no-workaround\n\ncondition: `(?i)\\b(?:workaround|quick fix|hack for now|temporary (?:fix|hack))\\b`\n\n",
    include_str!("ttsr_rules/atlas-v2/atlas-v2-no-workaround.md"),
    "\n#### atlas-v2-read-before-edit\n\nAlways applied; no condition.\n\n",
    include_str!("ttsr_rules/atlas-v2/atlas-v2-read-before-edit.md"),
    "\n### project-workflow\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| project-workflow-agents-md-binding | project-workflow/rules/project-workflow-agents-md-binding.md | always applied | - | - |\n",
    "| project-workflow-slice-first | project-workflow/rules/project-workflow-slice-first.md | text | prose-only | after-gap 10 |\n",
    "| project-workflow-write-it-down | project-workflow/rules/project-workflow-write-it-down.md | text | prose-only | after-gap 5 |\n\n",
    "#### project-workflow-agents-md-binding\n\nAlways applied; no condition.\n\n",
    include_str!("ttsr_rules/project-workflow/project-workflow-agents-md-binding.md"),
    "\n#### project-workflow-slice-first\n\ncondition: `(?i)\\b(?:rewrite|replace|restructure) (?:the )?(?:whole|entire|all of)\\b`\n\n",
    include_str!("ttsr_rules/project-workflow/project-workflow-slice-first.md"),
    "\n#### project-workflow-write-it-down\n\ncondition: `(?i)\\bi(?:'ll| will) (?:keep|remember) (?:that|this|it) in mind\\b`\n\n",
    include_str!("ttsr_rules/project-workflow/project-workflow-write-it-down.md"),
    "\n### git-commit\n\n",
    "| rule | source file | scope | interrupt | repeat |\n",
    "|---|---|---|---|---|\n",
    "| git-commit-no-force-push | git-commit/rules/git-commit-no-force-push.md | exec calls | tool-only | after-gap 1 |\n",
    "| git-commit-no-placeholder-message | git-commit/rules/git-commit-no-placeholder-message.md | exec calls | tool-only | after-gap 1 |\n",
    "| git-commit-no-secrets | git-commit/rules/git-commit-no-secrets.md | patch calls | tool-only | after-gap 1 |\n\n",
    "#### git-commit-no-force-push\n\ncondition: `(?i)(?:\\bgit push\\b.{0,200}\\s--force(?:\\s|$))|(?:\\bgit push\\b.{0,200}\\s-f(?:\\s|$))`\n\n",
    include_str!("ttsr_rules/git-commit/git-commit-no-force-push.md"),
    "\n#### git-commit-no-placeholder-message\n\ncondition: `(?i)\\bgit commit\\b.{0,200}-m\\s+[\"'](wip|tmp|temp|fix|update|stuff|changes|misc)[\"'](\\s|$)`\n\n",
    include_str!("ttsr_rules/git-commit/git-commit-no-placeholder-message.md"),
    "\n#### git-commit-no-secrets\n\ncondition: `(?i)(?:api[_-]?key|secret|password|token|credential)[\"']?\\s*[:=]\\s*[\"'][A-Za-z0-9+/_=-]{16,}[\"']`\n\n",
    include_str!("ttsr_rules/git-commit/git-commit-no-secrets.md"),
    "\n### detectors\n\n",
    "The `detectors` set is compiled code, not Markdown: three report-only\n",
    "lanes plus one text rule.\n",
    "`report: true` marks the v0 report-only state; the body text below is\n",
    "injected only after promotion, which is a dalgona release decision.\n",
    "The three streaming lanes fire once per turn at most, then latch. Only the `fabricated-unavailable-tool-call` text rule repeats `after-gap` with gap 1.\n\n",
    "| rule | kind | scope | body (used after promotion) |\n",
    "|---|---|---|---|\n",
    "| collapse-repetition | lane | text, thinking; all tool sources except the paragraph and near-duplicate sub-lanes | Your output fell into a repetition loop. Stop the loop, and continue with the next concrete step. |\n",
    "| control-token-leak | lane | text, thinking | Your output leaked control tokens. Stop echoing harness protocol text, and continue the task in plain prose. |\n",
    "| repetitive-turns | cross-turn lane | text | You have repeated a near-identical message across turns. Change approach, state what differs, and continue. |\n",
    "| fabricated-unavailable-tool-call | text rule | text | Your previous output imitated an unavailable tool call as inert text instead of taking action. Redo the interrupted step now with your real tools, such as patch for file changes. Do not print or imitate unavailable-tool transcript envelopes. |\n\n",
    "`fabricated-unavailable-tool-call` conditions, exactly, with interrupt\n",
    "mode `always`:\n\n",
    "- `(?i)<\\s*unavailable-tool-call\\b`\n",
    "- `(?i)\\[called\\s+tool\\s+[\"'][^\"'\\r\\n]+[\"']\\s+\\(no\\s+longer\\s+available\\s+in\\s+this\\s+session\\)`\n\n",
    "## Budgets\n\n",
    "The loader enforces four byte budgets in load order (user files, then\n",
    "each selected set, then project files). Instruction text is never\n",
    "truncated.\n\n",
    "- An always-apply body is at most 12288 bytes. A body over the cap\n",
    "  becomes a rulebook rule with the note `the body is longer than 12288 bytes. dalgona lists this rule in the rulebook instead.`\n",
    "- The always-apply prompt section holds at most 40960 bytes. When it is\n",
    "  full, the note is `the always-apply budget of 40960 bytes is full. dalgona lists this rule in the rulebook instead.`\n",
    "- One rulebook line is at most 4096 bytes. Over it, the note is `the rulebook line is longer than 4096 bytes. dalgona cut the line at 4096 bytes.` and the listing line is cut at 4096 bytes.\n",
    "- The whole rulebook listing is at most 10240 bytes. Over it, the note\n",
    "  is `the rulebook listing of 10240 bytes is full. dalgona did not list this rule; read it with rule://<name>.`\n\n",
    "A rule past a budget that has no description is skipped with the budget\n",
    "as the problem reason. Each skipped rule prints the startup notice once:\n",
    "`rules: dalgona skipped 1 part of your rules. Run \"dalgona rules\" to see why.`\n\n",
    "## Swapping and disabling\n\n",
    "- A user rule file with the same name replaces a bundled pack rule or a\n",
    "  project rule, never the reverse; the load note reads `rule \"<name>\" from plugin <set> is replaced by <path>`. The fired reminder uses the\n",
    "  user file's body and settings whole, including `report`.\n",
    "- `[rules] disabled = [\"<name>\"]` drops a rule with no problem row.\n",
    "- `[rule_sets] enabled = []` loads no bundled set; user and project\n",
    "  rules still load.\n",
    "- `[rules] watch = false` keeps always-apply and rulebook rules loading;\n",
    "  stream rules get the watch note `rules.watch is false, so dalgon does not watch this rule` and do not run; the detector lanes get `rules.watch is false, so dalgona does not watch this rule` and hold.\n",
    "- A user rule file named after a detector lane (for example\n",
    "  `collapse-repetition.md`) replaces the compiled lane when the session\n",
    "  builds its rules: the lane stands down and the user rule watches instead.\n",
    "- A rule with `enabled: false` in its front matter stays loaded but\n",
    "  inactive.\n",
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

    #[test]
    fn rules_doc_inventories_every_rule_with_origin_pattern_and_body() {
        for (set, rules) in PACK_RULES {
            assert!(RULES_DOC.contains(&format!("### {set}")), "{set}");
            for decl in *rules {
                let name = decl.name;
                let expected = table_row(set, decl);
                assert!(
                    RULES_DOC.contains(&expected),
                    "{name}: the table row does not match the declaration: {expected}"
                );
                if !decl.patterns.is_empty() {
                    for pattern in decl.patterns {
                        assert!(
                            RULES_DOC.contains(pattern),
                            "{name} declared pattern is missing from the page"
                        );
                    }
                }
                assert!(
                    RULES_DOC.contains(decl.file),
                    "{name} front matter and body are missing from the page"
                );
            }
        }
        for name in DETECTOR_RULES {
            assert!(RULES_DOC.contains(name), "{name} is missing from the page");
        }
        assert!(RULES_DOC.contains("plugin:ttsr-rules"));
        assert!(RULES_DOC.contains("plugin:quality"));

        for budget in [
            dal_ext::ttsr::build::BODY_OVER_LIMIT_NOTE,
            dal_ext::ttsr::build::ALWAYS_BUDGET_FULL_NOTE,
            "the rulebook line is longer than 4096 bytes. dalgona cut the line at 4096 bytes.",
            "the rulebook listing of 10240 bytes is full. dalgona did not list this rule; read it with rule://<name>.",
            dal_ext::ttsr::build::BODY_OVER_LIMIT_REASON,
            dal_ext::ttsr::build::ALWAYS_BUDGET_FULL_REASON,
            "rules: dalgona skipped 1 part of your rules. Run \"dalgona rules\" to see why.",
            "rules.watch is false, so dalgona does not watch this rule",
            "rules.watch is false, so dalgon does not watch this rule",
        ] {
            assert!(
                RULES_DOC.contains(budget),
                "{budget} is missing from the page"
            );
        }
    }

    #[test]
    fn rules_doc_mirrors_match_the_served_page() -> Result<(), std::io::Error> {
        let manifest = env!("CARGO_MANIFEST_DIR");
        for mirror in ["../docs/rules.md", "../docs/ttsr/rules.md"] {
            let text = std::fs::read_to_string(format!("{manifest}/{mirror}"))?;
            assert_eq!(text, RULES_DOC, "{mirror} drifted from the served page");
        }
        Ok(())
    }

    /// The inventory table row the page must hold for one declaration.
    fn table_row(set: &str, decl: &RuleDecl) -> String {
        let scope = match decl.scope {
            None => "always applied",
            Some(ScopeDecl::Text) => "text",
            Some(ScopeDecl::Tool(["patch"])) => "patch calls",
            Some(ScopeDecl::Tool(["exec"])) => "exec calls",
            Some(ScopeDecl::Tool(_)) => "tool calls",
        };
        let mode = match decl.mode {
            None => "-",
            Some(InterruptMode::ProseOnly) => "prose-only",
            Some(InterruptMode::ToolOnly) => "tool-only",
            Some(InterruptMode::Never) => "never",
            Some(_) => "always",
        };
        let repeat = decl
            .repeat_gap
            .map_or_else(|| "-".to_owned(), |gap| format!("after-gap {gap}"));
        format!(
            "| {} | {set}/rules/{}.md | {scope} | {mode} | {repeat} |",
            decl.name, decl.name
        )
    }
}
