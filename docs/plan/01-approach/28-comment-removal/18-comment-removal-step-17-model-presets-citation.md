#### Comment removal step 17. Model presets citation

Anchor: Christopher Alexander

- Files: `internal/model/presets.go`, `docs/spec/providers.md`.
- Change: delete the comment line of row 69 (`internal/model/presets.go:29`).
- Spec text, `docs/spec/providers.md`, under `## Capabilities and roles`: as a new paragraph after the paragraph that starts `Static role presets MUST match the decision table.`: The static preset identifiers MUST follow the native model guides of OpenAI, Anthropic, and Google, as cited in `docs/decisions/model-presets.md`, read 2026-11-08.
- Tests that fail before the change: none.
- Check: `just verify` passes.
- Lands after: Comment removal step 16

