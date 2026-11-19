#### Comment removal step 14. Anthropic model list citation

Anchor: John Carmack

- Files: `internal/model/anthropic/anthropic.go`, `docs/spec/providers.md`.
- Change: delete the comment line of row 65 (`internal/model/anthropic/anthropic.go:376`).
- Spec text, `docs/spec/providers.md`, under `## Capabilities and roles`: as a new paragraph after the paragraph that starts `` `Models` MUST use the provider list endpoint ``: The Anthropic model list decoder MUST read the fields of the List Models API at `https://platform.claude.com/docs/en/api/models/list`.
- Tests that fail before the change: none.
- Check: `just verify` passes.
- Lands after: Comment removal step 13

