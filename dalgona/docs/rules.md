# rules

Dalgona ships seven bundled rule sets plus the compiled detector lanes.
The `[rule_sets] enabled` list selects them; an unknown name fails
startup, and duplicate entries are idempotent.

| set | rules |
|---|---|
| steer | steer-no-apologies, steer-no-meta, steer-no-restate |
| compact | compact-cite-files, compact-no-context-loss, compact-state-on-disk |
| stop | stop-act-dont-offer, stop-evidence-before-done, stop-finish-the-work |
| docs | docs-contract-markers, docs-update-with-change |
| atlas-v2 | atlas-v2-no-workaround, atlas-v2-read-before-edit |
| project-workflow | project-workflow-agents-md-binding, project-workflow-slice-first, project-workflow-write-it-down |
| git-commit | git-commit-no-force-push, git-commit-no-placeholder-message, git-commit-no-secrets |
| detectors | collapse-repetition, control-token-leak, repetitive-turns, fabricated-unavailable-tool-call |
the shipped set registers{set}---
{set}stopstopnope
